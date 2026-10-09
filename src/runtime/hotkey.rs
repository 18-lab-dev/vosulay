use vosulay_frame::InputModifiers;

/// A manager hotkey the native host intercepts before the page sees the key.
///
/// The page cannot own this binding: while overlay interaction is active, keys
/// go to the overlay frame, so only the host can toggle interaction back off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct NativeHotkey {
    control: bool,
    alt: bool,
    shift: bool,
    meta: bool,
    /// Windows virtual-key code, which CEF reports on every platform.
    pub(super) code: i32,
}

impl NativeHotkey {
    /// Parses the manager's canonical `Ctrl+Alt+Shift+Meta+Key` form.
    ///
    /// Requires Ctrl, Alt, or Meta: a bare or Shift-only binding would be
    /// stolen from text fields because interception runs before the page.
    pub(super) fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('+').map(str::trim);
        let key = parts.next_back()?;
        let mut hotkey = Self {
            control: false,
            alt: false,
            shift: false,
            meta: false,
            code: key_code(key)?,
        };
        for modifier in parts {
            let flag = match modifier.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => &mut hotkey.control,
                "alt" => &mut hotkey.alt,
                "shift" => &mut hotkey.shift,
                "meta" | "cmd" | "command" | "super" | "win" => &mut hotkey.meta,
                _ => return None,
            };
            if std::mem::replace(flag, true) {
                return None;
            }
        }
        (hotkey.control || hotkey.alt || hotkey.meta).then_some(hotkey)
    }

    pub(super) fn matches(&self, modifiers: InputModifiers, code: i32) -> bool {
        main_key_code(code) == self.code
            && modifiers.control == self.control
            && modifiers.alt == self.alt
            && modifiers.shift == self.shift
            && modifiers.meta == self.meta
    }
}

/// The manager records keypad keys by their character, as the main-keyboard
/// key, so their distinct virtual-key codes must match that binding too.
fn main_key_code(code: i32) -> i32 {
    match code {
        0x60..=0x69 => code - 0x60 + 0x30,
        0x6B => 0xBB,
        0x6D => 0xBD,
        0x6E => 0xBE,
        0x6F => 0xBF,
        _ => code,
    }
}

fn key_code(key: &str) -> Option<i32> {
    let mut chars = key.chars();
    if let (Some(character), None) = (chars.next(), chars.next()) {
        return match character.to_ascii_uppercase() {
            letter @ 'A'..='Z' => Some(letter as i32),
            digit @ '0'..='9' => Some(digit as i32),
            ';' => Some(0xBA),
            ',' => Some(0xBC),
            '.' => Some(0xBE),
            '/' => Some(0xBF),
            '`' => Some(0xC0),
            '[' => Some(0xDB),
            '\\' => Some(0xDC),
            ']' => Some(0xDD),
            '\'' => Some(0xDE),
            _ => None,
        };
    }
    if let Some(number) = key
        .strip_prefix('F')
        .and_then(|number| number.parse::<i32>().ok())
        .filter(|number| (1..=24).contains(number))
    {
        return Some(0x6F + number);
    }
    Some(match key {
        "Backspace" => 0x08,
        "Tab" => 0x09,
        "Enter" => 0x0D,
        "Escape" => 0x1B,
        "Space" => 0x20,
        "PageUp" => 0x21,
        "PageDown" => 0x22,
        "End" => 0x23,
        "Home" => 0x24,
        "ArrowLeft" => 0x25,
        "ArrowUp" => 0x26,
        "ArrowRight" => 0x27,
        "ArrowDown" => 0x28,
        "Insert" => 0x2D,
        "Delete" => 0x2E,
        "Plus" => 0xBB,
        "Minus" => 0xBD,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modifiers(control: bool, alt: bool, shift: bool, meta: bool) -> InputModifiers {
        InputModifiers {
            control,
            alt,
            shift,
            meta,
            ..InputModifiers::default()
        }
    }

    #[test]
    fn matches_only_the_exact_modifier_set_and_key() {
        let hotkey = NativeHotkey::parse("Ctrl+Shift+I").unwrap();
        assert!(hotkey.matches(modifiers(true, false, true, false), 0x49));
        assert!(!hotkey.matches(modifiers(true, false, false, false), 0x49));
        assert!(!hotkey.matches(modifiers(true, true, true, false), 0x49));
        assert!(!hotkey.matches(modifiers(true, false, true, false), 0x4A));
    }

    #[test]
    fn keypad_keys_match_their_recorded_main_keyboard_binding() {
        let held = modifiers(true, false, false, false);
        for (text, keypad) in [
            ("Ctrl+Plus", 0x6B),
            ("Ctrl+Minus", 0x6D),
            ("Ctrl+/", 0x6F),
            ("Ctrl+.", 0x6E),
            ("Ctrl+0", 0x60),
            ("Ctrl+9", 0x69),
        ] {
            assert!(
                NativeHotkey::parse(text).unwrap().matches(held, keypad),
                "{text}"
            );
        }
        assert!(
            !NativeHotkey::parse("Ctrl+Plus")
                .unwrap()
                .matches(held, 0x6A)
        );
    }

    #[test]
    fn maps_function_named_and_punctuation_keys_to_virtual_key_codes() {
        for (text, code) in [
            ("Alt+F1", 0x70),
            ("Ctrl+F24", 0x87),
            ("Meta+Space", 0x20),
            ("Ctrl+ArrowDown", 0x28),
            ("Ctrl+Plus", 0xBB),
            ("Ctrl+[", 0xDB),
            ("Ctrl+7", 0x37),
        ] {
            assert_eq!(
                NativeHotkey::parse(text).map(|key| key.code),
                Some(code),
                "{text}"
            );
        }
    }

    #[test]
    fn rejects_unmappable_unmodified_or_malformed_bindings() {
        for text in [
            "I",
            "Shift+I",
            "Ctrl+!",
            "Ctrl+F25",
            "Ctrl+Ctrl+I",
            "Hyper+I",
            "Ctrl+",
            "",
        ] {
            assert_eq!(NativeHotkey::parse(text), None, "{text}");
        }
    }
}
