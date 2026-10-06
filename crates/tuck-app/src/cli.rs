//! Command line (DESIGN §8): `tuck`, `--background`, `--clipboard`, `--emoji`, `--settings`, `--install`,
//! `--uninstall [--purge]`, `--quit`, `--selftest [--json]`, `--preview <view> --out <png> [--theme] [--scale]`,
//! `--version`, `--help`. Unknown arguments are collected so the caller can log and ignore them.

use std::path::PathBuf;

use tuck_core::ThemeMode;

pub const USAGE: &str = "\
Tuck: clipboard history and emoji in one panel.

  tuck                    start in the tray (Win+V and Win+. open the panel)
  tuck --background       start silently (used at login)
  tuck --clipboard        open the panel on Clipboard
  tuck --emoji            open the panel on Emoji
  tuck --settings         open the settings window
  tuck --install          install for this user and start
  tuck --uninstall        remove Tuck (keeps history and settings)
  tuck --uninstall --purge  remove Tuck and its data
  tuck --quit             quit the running Tuck
  tuck --selftest [--json]
  tuck --preview <view> --out <png> [--theme dark|light] [--scale 1|1.5|2]
  tuck --version | --help";

/// What a resident start says to the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Greeting {
    /// `--background`: start silently.
    Silent,
    /// Plain `tuck`: "Tuck is running".
    Running,
    /// Started by `--install`: "Tuck is installed".
    Installed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PreviewArgs {
    pub kind: String,
    pub out: PathBuf,
    pub theme: ThemeMode,
    pub scale: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Start(Greeting),
    Clipboard,
    Emoji,
    Settings,
    /// Ask the running instance to quit.
    Quit,
    Install,
    Uninstall {
        purge: bool,
    },
    Selftest {
        json: bool,
    },
    Preview(PreviewArgs),
    Version,
    Help,
}

impl Command {
    /// Commands that run in the resident instance (forwarded to it when one is running).
    pub fn is_resident(&self) -> bool {
        matches!(
            self,
            Command::Start(_)
                | Command::Clipboard
                | Command::Emoji
                | Command::Settings
                | Command::Quit
                | Command::Uninstall { purge: false }
        )
    }

    /// `--uninstall --purge` deletes the folder the log lives in, so that process must not open the log.
    pub fn keeps_log_closed(&self) -> bool {
        matches!(self, Command::Uninstall { purge: true })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cli {
    pub command: Command,
    /// Arguments that were not understood; logged and ignored.
    pub ignored: Vec<String>,
}

/// Internal flag added by `--install` when it starts the installed copy.
pub const INSTALLED_FLAG: &str = "--installed";

fn parse_theme(word: &str) -> Option<ThemeMode> {
    match word.to_ascii_lowercase().as_str() {
        "dark" => Some(ThemeMode::Dark),
        "light" => Some(ThemeMode::Light),
        _ => None,
    }
}

fn parse_scale(word: &str) -> Option<f32> {
    word.parse::<f32>().ok().filter(|s| s.is_finite() && (0.5..=4.0).contains(s))
}

pub fn parse(args: &[String]) -> Cli {
    let mut command: Option<Command> = None;
    let mut ignored = Vec::new();
    let mut background = false;
    let mut installed = false;
    let mut json = false;
    let mut purge = false;
    let mut preview_kind: Option<String> = None;
    let mut out: Option<PathBuf> = None;
    let mut theme = ThemeMode::Dark;
    let mut scale = 1.0;
    let mut iter = args.iter().peekable();
    let set = |command: &mut Option<Command>, ignored: &mut Vec<String>, arg: &str, value: Command| {
        if command.is_none() {
            *command = Some(value);
        } else {
            ignored.push(arg.to_string());
        }
    };
    while let Some(arg) = iter.next() {
        let lower = arg.to_ascii_lowercase();
        match lower.as_str() {
            "--background" => background = true,
            INSTALLED_FLAG => installed = true,
            "--clipboard" => set(&mut command, &mut ignored, arg, Command::Clipboard),
            "--emoji" => set(&mut command, &mut ignored, arg, Command::Emoji),
            "--settings" => set(&mut command, &mut ignored, arg, Command::Settings),
            "--quit" | "--exit" => set(&mut command, &mut ignored, arg, Command::Quit),
            "--install" => set(&mut command, &mut ignored, arg, Command::Install),
            "--uninstall" => set(&mut command, &mut ignored, arg, Command::Uninstall { purge: false }),
            "--purge" => purge = true,
            "--version" | "-v" | "-V" => set(&mut command, &mut ignored, arg, Command::Version),
            "--help" | "-h" | "-?" | "/?" => set(&mut command, &mut ignored, arg, Command::Help),
            "--selftest" => set(&mut command, &mut ignored, arg, Command::Selftest { json: false }),
            "--json" => json = true,
            "--preview" => {
                match iter.peek().filter(|next| !next.starts_with("--")) {
                    Some(kind) => {
                        preview_kind = Some(kind.to_string());
                        iter.next();
                    }
                    None => preview_kind = Some(String::new()),
                }
                set(&mut command, &mut ignored, arg, Command::Preview(placeholder_preview()));
            }
            "--out" => match iter.next() {
                Some(path) => out = Some(PathBuf::from(path)),
                None => ignored.push(arg.clone()),
            },
            "--theme" => match iter.next().and_then(|w| parse_theme(w)) {
                Some(t) => theme = t,
                None => ignored.push(arg.clone()),
            },
            "--scale" => match iter.next().and_then(|w| parse_scale(w)) {
                Some(s) => scale = s,
                None => ignored.push(arg.clone()),
            },
            _ => ignored.push(arg.clone()),
        }
    }
    let command = match command {
        Some(Command::Selftest { .. }) => Command::Selftest { json },
        Some(Command::Uninstall { .. }) => Command::Uninstall { purge },
        Some(Command::Preview(_)) => {
            let kind = preview_kind.unwrap_or_default();
            let out = out.unwrap_or_else(|| PathBuf::from(format!("{kind}.png")));
            Command::Preview(PreviewArgs { kind, out, theme, scale })
        }
        Some(other) => other,
        None if installed => Command::Start(Greeting::Installed),
        None if background => Command::Start(Greeting::Silent),
        None => Command::Start(Greeting::Running),
    };
    if purge && !matches!(command, Command::Uninstall { .. }) {
        ignored.push("--purge".into());
    }
    Cli { command, ignored }
}

fn placeholder_preview() -> PreviewArgs {
    PreviewArgs { kind: String::new(), out: PathBuf::new(), theme: ThemeMode::Dark, scale: 1.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli(args: &[&str]) -> Cli {
        parse(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn start_variants() {
        assert_eq!(cli(&[]).command, Command::Start(Greeting::Running));
        assert_eq!(cli(&["--background"]).command, Command::Start(Greeting::Silent));
        assert_eq!(cli(&["--background", "--installed"]).command, Command::Start(Greeting::Installed));
    }

    #[test]
    fn panel_and_settings_commands() {
        assert_eq!(cli(&["--clipboard"]).command, Command::Clipboard);
        assert_eq!(cli(&["--EMOJI"]).command, Command::Emoji);
        assert_eq!(cli(&["--settings"]).command, Command::Settings);
        assert!(Command::Clipboard.is_resident() && Command::Emoji.is_resident() && Command::Settings.is_resident());
    }

    #[test]
    fn uninstall_and_purge() {
        assert_eq!(cli(&["--uninstall"]).command, Command::Uninstall { purge: false });
        assert_eq!(cli(&["--purge", "--uninstall"]).command, Command::Uninstall { purge: true });
        assert!(Command::Uninstall { purge: false }.is_resident());
        assert!(!Command::Uninstall { purge: true }.is_resident());
        assert!(Command::Uninstall { purge: true }.keeps_log_closed());
        let stray = cli(&["--purge"]);
        assert_eq!(stray.command, Command::Start(Greeting::Running));
        assert_eq!(stray.ignored, vec!["--purge".to_string()]);
    }

    #[test]
    fn preview_with_options() {
        let parsed = cli(&["--preview", "panel-emoji", "--out", "x.png", "--theme", "light", "--scale", "1.5"]);
        assert_eq!(
            parsed.command,
            Command::Preview(PreviewArgs {
                kind: "panel-emoji".into(),
                out: PathBuf::from("x.png"),
                theme: ThemeMode::Light,
                scale: 1.5,
            })
        );
        assert!(parsed.ignored.is_empty());
        let defaults = cli(&["--out", "y.png", "--preview", "toast"]);
        assert_eq!(
            defaults.command,
            Command::Preview(PreviewArgs {
                kind: "toast".into(),
                out: "y.png".into(),
                theme: ThemeMode::Dark,
                scale: 1.0
            })
        );
        assert_eq!(cli(&["--preview", "toast", "--scale", "9"]).ignored, vec!["--scale".to_string()]);
    }

    #[test]
    fn selftest_json_and_one_command_wins() {
        assert_eq!(cli(&["--selftest", "--json"]).command, Command::Selftest { json: true });
        assert_eq!(cli(&["--json", "--selftest"]).command, Command::Selftest { json: true });
        let parsed = cli(&["--emoji", "--settings", "--frobnicate"]);
        assert_eq!(parsed.command, Command::Emoji);
        assert_eq!(parsed.ignored, vec!["--settings".to_string(), "--frobnicate".to_string()]);
        assert_eq!(cli(&["--version"]).command, Command::Version);
        assert_eq!(cli(&["--help"]).command, Command::Help);
        assert_eq!(cli(&["--quit"]).command, Command::Quit);
        assert!(!Command::Install.is_resident() && Command::Quit.is_resident());
    }
}
