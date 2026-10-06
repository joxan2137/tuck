use crate::hook::{MARKER, vk};
use anyhow::{Result, ensure};
use std::time::{Duration, Instant};
use tuck_core::PasteKey;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, MAPVK_VK_TO_VSC, MapVirtualKeyW, SendInput, VIRTUAL_KEY,
};

const MODIFIER_WAIT: Duration = Duration::from_millis(300);
const MODIFIER_POLL: Duration = Duration::from_millis(5);
const HELD_MODIFIERS: [u16; 8] =
    [vk::LWIN, vk::RWIN, vk::LMENU, vk::RMENU, vk::LCONTROL, vk::RCONTROL, vk::LSHIFT, vk::RSHIFT];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stroke {
    Key { vk: u16, up: bool },
    Unit { unit: u16, up: bool },
}

fn tap(vk: u16) -> [Stroke; 2] {
    [Stroke::Key { vk, up: false }, Stroke::Key { vk, up: true }]
}

/// Key-ups for the modifiers still down (never re-pressed later). The mask key goes first when Win or Alt is
/// among them, so their release opens neither Start nor a menu bar.
fn release_plan(is_down: impl Fn(u16) -> bool) -> Vec<Stroke> {
    let held: Vec<u16> = HELD_MODIFIERS.into_iter().filter(|&vk| is_down(vk)).collect();
    let mut plan = Vec::new();
    if held.iter().any(|&vk| matches!(vk, vk::LWIN | vk::RWIN | vk::LMENU | vk::RMENU)) {
        plan.extend(tap(vk::MASK));
    }
    plan.extend(held.into_iter().map(|vk| Stroke::Key { vk, up: true }));
    plan
}

/// One down/up pair per UTF-16 unit; line breaks (`\r\n`, `\n`, `\r`) become Enter.
fn text_strokes(text: &str) -> Vec<Stroke> {
    let mut strokes = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' || c == '\n' {
            if c == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            strokes.extend(tap(vk::RETURN));
            continue;
        }
        let mut units = [0; 2];
        for &unit in c.encode_utf16(&mut units).iter() {
            strokes.extend([Stroke::Unit { unit, up: false }, Stroke::Unit { unit, up: true }]);
        }
    }
    strokes
}

fn paste_strokes(key: PasteKey) -> Vec<Stroke> {
    let (modifiers, trigger): (&[u16], u16) = match key {
        PasteKey::CtrlV => (&[vk::LCONTROL], vk::KEY_V),
        PasteKey::ShiftInsert => (&[vk::LSHIFT], vk::INSERT),
        PasteKey::CtrlShiftV => (&[vk::LCONTROL, vk::LSHIFT], vk::KEY_V),
    };
    let mut strokes: Vec<Stroke> = modifiers.iter().map(|&vk| Stroke::Key { vk, up: false }).collect();
    strokes.extend(tap(trigger));
    strokes.extend(modifiers.iter().rev().map(|&vk| Stroke::Key { vk, up: true }));
    strokes
}

fn is_extended(vk: u16) -> bool {
    matches!(vk, vk::RCONTROL | vk::RMENU | vk::LWIN | vk::RWIN | vk::INSERT)
}

fn input(stroke: Stroke) -> INPUT {
    let up = |up: bool| if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) };
    let ki = match stroke {
        Stroke::Key { vk, up: is_up } => KEYBDINPUT {
            wVk: VIRTUAL_KEY(vk),
            wScan: unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as u16,
            dwFlags: up(is_up) | if is_extended(vk) { KEYEVENTF_EXTENDEDKEY } else { KEYBD_EVENT_FLAGS(0) },
            dwExtraInfo: MARKER,
            ..Default::default()
        },
        Stroke::Unit { unit, up: is_up } => KEYBDINPUT {
            wVk: VIRTUAL_KEY(0),
            wScan: unit,
            dwFlags: KEYEVENTF_UNICODE | up(is_up),
            dwExtraInfo: MARKER,
            ..Default::default()
        },
    };
    INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki } }
}

fn send(strokes: &[Stroke]) -> Result<()> {
    if strokes.is_empty() {
        return Ok(());
    }
    let inputs: Vec<INPUT> = strokes.iter().map(|&stroke| input(stroke)).collect();
    let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
    ensure!(
        sent as usize == inputs.len(),
        "SendInput inserted {sent} of {} events: {}",
        inputs.len(),
        windows::core::Error::from_thread()
    );
    Ok(())
}

fn wait_for_modifier_release() {
    let deadline = Instant::now() + MODIFIER_WAIT;
    while HELD_MODIFIERS.iter().any(|&vk| crate::hook::key_is_down(vk)) && Instant::now() < deadline {
        std::thread::sleep(MODIFIER_POLL);
    }
}

pub(crate) fn send_mask() {
    let _ = send(&tap(vk::MASK));
}

/// Types `text` into the focused window as Unicode key events. Blocks up to 300 ms while the user lets go of
/// Shift/Ctrl/Alt/Win. A higher-integrity target silently ignores the input (check `Target::elevated`).
pub fn type_text(text: &str) -> Result<()> {
    wait_for_modifier_release();
    let mut strokes = release_plan(crate::hook::key_is_down);
    strokes.extend(text_strokes(text));
    send(&strokes)
}

/// Sends Ctrl+V, Shift+Insert or Ctrl+Shift+V with the same modifier handling as `type_text`.
pub fn paste(key: PasteKey) -> Result<()> {
    wait_for_modifier_release();
    let mut strokes = release_plan(crate::hook::key_is_down);
    strokes.extend(paste_strokes(key));
    send(&strokes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn up(vk: u16) -> Stroke {
        Stroke::Key { vk, up: true }
    }

    fn down(vk: u16) -> Stroke {
        Stroke::Key { vk, up: false }
    }

    #[test]
    fn nothing_held_needs_no_release() {
        assert!(release_plan(|_| false).is_empty());
    }

    #[test]
    fn only_held_modifiers_are_released() {
        assert_eq!(release_plan(|vk| vk == vk::LSHIFT), [up(vk::LSHIFT)]);
        assert_eq!(release_plan(|vk| matches!(vk, vk::RCONTROL | vk::LSHIFT)), [up(vk::RCONTROL), up(vk::LSHIFT)]);
    }

    #[test]
    fn mask_precedes_win_or_alt_release() {
        for held in [vk::LWIN, vk::RWIN, vk::LMENU, vk::RMENU] {
            assert_eq!(release_plan(|vk| vk == held), [down(vk::MASK), up(vk::MASK), up(held)]);
        }
        assert_eq!(
            release_plan(|vk| matches!(vk, vk::LWIN | vk::LCONTROL)),
            [down(vk::MASK), up(vk::MASK), up(vk::LWIN), up(vk::LCONTROL)]
        );
    }

    #[test]
    fn plans_never_press_a_modifier() {
        let plan = release_plan(|vk| HELD_MODIFIERS.contains(&vk));
        assert!(
            plan.iter().all(|stroke| matches!(stroke, Stroke::Key { up: true, .. } | Stroke::Key { vk: vk::MASK, .. }))
        );
    }

    #[test]
    fn text_is_unicode_units_with_enter_for_line_breaks() {
        let unit = |unit, up| Stroke::Unit { unit, up };
        assert_eq!(text_strokes("é"), [unit(0xe9, false), unit(0xe9, true)]);
        assert_eq!(
            text_strokes("😀"),
            [unit(0xd83d, false), unit(0xd83d, true), unit(0xde00, false), unit(0xde00, true)]
        );
        assert_eq!(
            text_strokes("a\r\nb\nc\r"),
            [
                unit(0x61, false),
                unit(0x61, true),
                down(vk::RETURN),
                up(vk::RETURN),
                unit(0x62, false),
                unit(0x62, true),
                down(vk::RETURN),
                up(vk::RETURN),
                unit(0x63, false),
                unit(0x63, true),
                down(vk::RETURN),
                up(vk::RETURN),
            ]
        );
    }

    #[test]
    fn paste_chords() {
        assert_eq!(
            paste_strokes(PasteKey::CtrlV),
            [down(vk::LCONTROL), down(vk::KEY_V), up(vk::KEY_V), up(vk::LCONTROL)]
        );
        assert_eq!(
            paste_strokes(PasteKey::ShiftInsert),
            [down(vk::LSHIFT), down(vk::INSERT), up(vk::INSERT), up(vk::LSHIFT)]
        );
        assert_eq!(
            paste_strokes(PasteKey::CtrlShiftV),
            [down(vk::LCONTROL), down(vk::LSHIFT), down(vk::KEY_V), up(vk::KEY_V), up(vk::LSHIFT), up(vk::LCONTROL)]
        );
    }

    #[test]
    fn inputs_are_tagged_and_flagged() {
        let insert = input(down(vk::INSERT));
        let unit = input(Stroke::Unit { unit: 0x41, up: true });
        unsafe {
            assert_eq!(insert.Anonymous.ki.dwExtraInfo, MARKER);
            assert!(insert.Anonymous.ki.dwFlags.contains(KEYEVENTF_EXTENDEDKEY));
            assert_eq!(unit.Anonymous.ki.wVk, VIRTUAL_KEY(0));
            assert_eq!(unit.Anonymous.ki.dwFlags, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP);
            assert_eq!(unit.Anonymous.ki.dwExtraInfo, MARKER);
        }
    }
}
