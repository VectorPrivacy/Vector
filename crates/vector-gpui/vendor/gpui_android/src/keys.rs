//! Android `KeyEvent`s as GPUI keystrokes.

use gpui::{Keystroke, Modifiers};

const META_SHIFT_ON: i32 = 0x1;
const META_ALT_ON: i32 = 0x2;
const META_CTRL_ON: i32 = 0x1000;
const META_META_ON: i32 = 0x10000;
const META_FUNCTION_ON: i32 = 0x8;
pub(crate) const META_CAPS_LOCK_ON: i32 = 0x100000;

pub(crate) fn modifiers(meta: i32) -> Modifiers {
    Modifiers {
        control: meta & META_CTRL_ON != 0,
        alt: meta & META_ALT_ON != 0,
        shift: meta & META_SHIFT_ON != 0,
        platform: meta & META_META_ON != 0,
        function: meta & META_FUNCTION_ON != 0,
    }
}

/// GPUI's name for a key that types nothing, by `KeyEvent.KEYCODE_*`.
fn named(key_code: i32) -> Option<&'static str> {
    Some(match key_code {
        19 => "up",
        20 => "down",
        21 => "left",
        22 => "right",
        61 => "tab",
        62 => "space",
        66 | 160 => "enter",
        67 => "backspace",
        92 => "pageup",
        93 => "pagedown",
        111 => "escape",
        112 => "delete",
        122 => "home",
        123 => "end",
        124 => "insert",
        131 => "f1",
        132 => "f2",
        133 => "f3",
        134 => "f4",
        135 => "f5",
        136 => "f6",
        137 => "f7",
        138 => "f8",
        139 => "f9",
        140 => "f10",
        141 => "f11",
        142 => "f12",
        _ => return None,
    })
}

/// `unicode` is what the key types with its modifiers, `base` what it types with none.
pub(crate) fn keystroke(key_code: i32, unicode: u32, base: u32, meta: i32) -> Option<Keystroke> {
    let modifiers = modifiers(meta);
    let typed = char::from_u32(unicode).filter(|c| !c.is_control());
    if let Some(name) = named(key_code) {
        let key_char = match name {
            "space" => Some(" ".to_string()),
            "enter" => Some("\n".to_string()),
            "tab" => Some("\t".to_string()),
            _ => None,
        };
        return Some(Keystroke { modifiers, key: name.to_string(), key_char });
    }
    let base = char::from_u32(base).filter(|c| !c.is_control()).or(typed)?;
    Some(Keystroke {
        modifiers,
        key: base.to_lowercase().collect(),
        key_char: typed.map(String::from),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_carry_what_they_type() {
        let k = keystroke(29, 'A' as u32, 'a' as u32, META_SHIFT_ON).unwrap();
        assert_eq!(k.key, "a");
        assert_eq!(k.key_char.as_deref(), Some("A"));
        assert!(k.modifiers.shift);
    }

    #[test]
    fn named_keys_type_nothing_but_whitespace() {
        assert_eq!(keystroke(67, 0, 0, 0).unwrap().key, "backspace");
        assert_eq!(keystroke(66, 10, 10, 0).unwrap().key_char.as_deref(), Some("\n"));
        assert!(keystroke(0, 0, 0, 0).is_none());
    }
}
