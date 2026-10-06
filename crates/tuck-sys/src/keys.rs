use crate::hook::{RoutedKey, vk};
use windows::Win32::{
    Globalization::{NormalizationC, NormalizeString},
    UI::Input::KeyboardAndMouse::{HKL, ToUnicodeEx},
};

/// ToUnicodeEx flag: translate without touching the kernel keyboard state (no dead-key side effects).
const KEEP_KEYBOARD_STATE: u32 = 0x4;
const KEY_PRESSED: u8 = 0x80;
const KEY_TOGGLED: u8 = 0x01;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyText {
    None,
    Text(String),
    /// A dead key is pending; the next key completes it.
    Dead,
}

/// What `ToUnicodeEx` produced for one key.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Translation {
    Nothing,
    Dead(char),
    Chars(String),
}

/// Turns routed keys into text with the target's keyboard layout. Holds the pending dead key.
#[derive(Debug, Default)]
pub struct TextTranslator {
    pending_dead: Option<char>,
    pending_high_surrogate: Option<u16>,
}
impl TextTranslator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// `hkl` is `Target::layout`. Key-ups yield `KeyText::None`.
    pub fn translate(&mut self, key: &RoutedKey, hkl: isize) -> KeyText {
        if !key.down {
            return KeyText::None;
        }
        if key.vk == vk::PACKET {
            return self.packet_unit(key.scan);
        }
        let (text, pending) = combine(self.pending_dead, to_unicode(key, hkl));
        self.pending_dead = pending;
        text
    }

    /// Characters other programs inject as VK_PACKET carry their UTF-16 unit in the scan code.
    fn packet_unit(&mut self, unit: u16) -> KeyText {
        match unit {
            0xd800..=0xdbff => {
                self.pending_high_surrogate = Some(unit);
                KeyText::None
            }
            0xdc00..=0xdfff => match self.pending_high_surrogate.take() {
                Some(high) => KeyText::Text(String::from_utf16_lossy(&[high, unit])),
                None => KeyText::None,
            },
            _ => {
                self.pending_high_surrogate = None;
                char::from_u32(unit.into())
                    .filter(|c| !c.is_control())
                    .map_or(KeyText::None, |c| KeyText::Text(c.to_string()))
            }
        }
    }
}

fn key_state(key: &RoutedKey) -> [u8; 256] {
    let mut state = [0u8; 256];
    let mut press = |keys: &[u16]| {
        for &key in keys {
            state[key as usize] = KEY_PRESSED;
        }
    };
    if key.mods.shift {
        press(&[vk::SHIFT, vk::LSHIFT]);
    }
    if key.mods.ctrl {
        press(&[vk::CONTROL, vk::LCONTROL]);
    }
    if key.mods.alt {
        press(&[vk::MENU, vk::LMENU]);
    }
    if key.mods.altgr {
        press(&[vk::CONTROL, vk::LCONTROL, vk::MENU, vk::RMENU]);
    }
    if key.caps_lock {
        state[vk::CAPITAL as usize] = KEY_TOGGLED;
    }
    state
}

fn to_unicode(key: &RoutedKey, hkl: isize) -> Translation {
    let mut buffer = [0u16; 16];
    let count = unsafe {
        ToUnicodeEx(
            key.vk.into(),
            key.scan.into(),
            &key_state(key),
            &mut buffer,
            KEEP_KEYBOARD_STATE,
            Some(HKL(hkl as *mut _)),
        )
    };
    match count {
        ..0 => char::from_u32(buffer[0].into()).map_or(Translation::Nothing, Translation::Dead),
        0 => Translation::Nothing,
        _ => Translation::Chars(String::from_utf16_lossy(&buffer[..(count as usize).min(buffer.len())])),
    }
}

/// Dead-key composition: returns the text for this key and the dead key still pending afterwards.
fn combine(pending: Option<char>, translation: Translation) -> (KeyText, Option<char>) {
    match (pending, translation) {
        (pending, Translation::Nothing) => (KeyText::None, pending),
        (None, Translation::Dead(dead)) => (KeyText::Dead, Some(dead)),
        (Some(previous), Translation::Dead(dead)) => (KeyText::Text(format!("{previous}{dead}")), None),
        (pending, Translation::Chars(chars)) => {
            let printable: String = chars.chars().filter(|c| !c.is_control()).collect();
            match pending {
                _ if printable.is_empty() => (KeyText::None, None),
                None => (KeyText::Text(printable), None),
                Some(dead) if printable == " " => (KeyText::Text(dead.to_string()), None),
                Some(dead) => {
                    (KeyText::Text(compose(dead, &printable).unwrap_or_else(|| format!("{dead}{printable}"))), None)
                }
            }
        }
    }
}

fn compose(dead: char, base: &str) -> Option<String> {
    let mut chars = base.chars();
    let (Some(first), None) = (chars.next(), chars.next()) else {
        return None;
    };
    let composed = nfc(&format!("{first}{}", combining_marks(dead)?))?;
    (composed.chars().count() == 1).then_some(composed)
}

/// The combining mark(s) a layout's spacing dead-key character stands for.
fn combining_marks(dead: char) -> Option<String> {
    let marks = match dead {
        '\u{300}'..='\u{36f}' => return Some(dead.to_string()),
        '`' => "\u{300}",
        '´' | '\'' | '΄' => "\u{301}",
        '^' | 'ˆ' => "\u{302}",
        '~' | '˜' => "\u{303}",
        '¯' => "\u{304}",
        '˘' => "\u{306}",
        '˙' => "\u{307}",
        '¨' | '"' => "\u{308}",
        '˚' | '°' => "\u{30a}",
        '˝' => "\u{30b}",
        'ˇ' => "\u{30c}",
        '¸' => "\u{327}",
        '˛' => "\u{328}",
        '΅' => "\u{308}\u{301}",
        _ => return None,
    };
    Some(marks.to_string())
}

fn nfc(text: &str) -> Option<String> {
    let source: Vec<u16> = text.encode_utf16().collect();
    let mut buffer = vec![0u16; source.len() * 3 + 8];
    let length = unsafe { NormalizeString(NormalizationC, &source, Some(&mut buffer)) };
    (length > 0).then(|| String::from_utf16_lossy(&buffer[..length as usize]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hook::Mods;

    fn chars(text: &str) -> Translation {
        Translation::Chars(text.into())
    }

    fn text(value: &str) -> KeyText {
        KeyText::Text(value.into())
    }

    #[test]
    fn dead_keys_compose_with_the_next_letter() {
        for (dead, base, expected) in [
            ('^', "e", "ê"),
            ('´', "E", "É"),
            ('\'', "a", "á"),
            ('`', "a", "à"),
            ('¨', "u", "ü"),
            ('"', "o", "ö"),
            ('~', "n", "ñ"),
            ('ˇ', "c", "č"),
            ('˚', "a", "å"),
            ('¸', "c", "ç"),
            ('˛', "e", "ę"),
            ('΄', "α", "ά"),
            ('΅', "ι", "ΐ"),
            ('\u{301}', "o", "ó"),
        ] {
            assert_eq!(combine(Some(dead), chars(base)), (text(expected), None), "{dead} + {base}");
        }
    }

    #[test]
    fn dead_key_alone_is_pending() {
        assert_eq!(combine(None, Translation::Dead('^')), (KeyText::Dead, Some('^')));
        assert_eq!(combine(Some('^'), Translation::Nothing), (KeyText::None, Some('^')));
    }

    #[test]
    fn dead_plus_space_or_uncombinable_yields_the_dead_char_first() {
        assert_eq!(combine(Some('^'), chars(" ")), (text("^"), None));
        assert_eq!(combine(Some('^'), chars("x")), (text("^x"), None));
        assert_eq!(combine(Some('ˇ'), chars("1")), (text("ˇ1"), None));
        assert_eq!(combine(Some('^'), chars("ab")), (text("^ab"), None));
        assert_eq!(combine(Some('^'), Translation::Dead('^')), (text("^^"), None));
        assert_eq!(combine(Some('§'), chars("a")), (text("§a"), None));
    }

    #[test]
    fn control_characters_are_not_text() {
        for control in ["\r", "\u{8}", "\t", "\u{1b}", "\u{1}", "\u{7f}"] {
            assert_eq!(combine(None, chars(control)), (KeyText::None, None));
            assert_eq!(combine(Some('^'), chars(control)), (KeyText::None, None));
        }
        assert_eq!(combine(None, chars("a")), (text("a"), None));
        assert_eq!(combine(None, chars("😀")), (text("😀"), None));
    }

    #[test]
    fn key_state_reflects_mods_and_caps_lock() {
        let key = |mods, caps_lock| RoutedKey {
            vk: vk::KEY_A,
            scan: 0x1e,
            extended: false,
            down: true,
            repeat: false,
            mods,
            caps_lock,
        };
        let state = key_state(&key(Mods { shift: true, ..Mods::default() }, true));
        assert_eq!((state[vk::SHIFT as usize], state[vk::CAPITAL as usize]), (KEY_PRESSED, KEY_TOGGLED));
        assert_eq!(state[vk::CONTROL as usize], 0);
        let state = key_state(&key(Mods { altgr: true, ..Mods::default() }, false));
        for pressed in [vk::CONTROL, vk::MENU, vk::RMENU] {
            assert_eq!(state[pressed as usize], KEY_PRESSED);
        }
        assert_eq!(state[vk::LMENU as usize], 0);
    }

    #[test]
    fn packets_and_key_ups() {
        let packet = |scan, down| RoutedKey {
            vk: vk::PACKET,
            scan,
            extended: false,
            down,
            repeat: false,
            mods: Mods::default(),
            caps_lock: false,
        };
        let mut translator = TextTranslator::new();
        assert_eq!(translator.translate(&packet(0xe9, true), 0), text("é"));
        assert_eq!(translator.translate(&packet(0xe9, false), 0), KeyText::None);
        assert_eq!(translator.translate(&packet(0xd83d, true), 0), KeyText::None);
        assert_eq!(translator.translate(&packet(0xde00, true), 0), text("😀"));
        assert_eq!(translator.translate(&packet(0xde00, true), 0), KeyText::None);
        assert_eq!(translator.translate(&packet(0x0d, true), 0), KeyText::None);
    }
}
