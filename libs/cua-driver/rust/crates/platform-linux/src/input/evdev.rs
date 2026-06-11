//! Key-name → Linux evdev keycode mapping for the real-input tier.
//!
//! `ydotool key` (and a future direct `/dev/uinput` path) speak evdev
//! keycodes from `linux/input-event-codes.h`, NOT X11 keysyms — so this is
//! a separate table from the XSendEvent path's keysym map. Covers the
//! navigation/editing/function keys and the ASCII printables an agent
//! drives; unknown names return `None` so the caller can error cleanly.

/// evdev keycode for a named key (case-insensitive). Names match the
/// XSendEvent path's vocabulary so `dispatch:"real"` accepts the same
/// `key` strings.
pub fn keycode(name: &str) -> Option<u16> {
    let n = name.to_lowercase();
    Some(match n.as_str() {
        "enter" | "return" => 28,
        "esc" | "escape" => 1,
        "tab" => 15,
        "space" | " " => 57,
        "backspace" => 14,
        "delete" | "del" => 111,
        "insert" | "ins" => 110,
        "home" => 102,
        "end" => 107,
        "pageup" | "pgup" => 104,
        "pagedown" | "pgdn" => 109,
        "up" => 103,
        "down" => 108,
        "left" => 105,
        "right" => 106,
        "minus" | "-" => 12,
        "equal" | "=" => 13,
        "comma" | "," => 51,
        "period" | "." => 52,
        "slash" | "/" => 53,
        "semicolon" | ";" => 39,
        "apostrophe" | "'" => 40,
        "leftbracket" | "[" => 26,
        "rightbracket" | "]" => 27,
        "backslash" | "\\" => 43,
        "grave" | "`" => 41,
        _ => {
            if let Some(c) = single_char(&n) {
                return char_keycode(c).map(|(kc, _shift)| kc);
            }
            return function_key(&n);
        }
    })
}

/// evdev keycode for a modifier name (left-hand variants).
pub fn modifier_keycode(name: &str) -> Option<u16> {
    Some(match name.to_lowercase().as_str() {
        "ctrl" | "control" => 29,   // KEY_LEFTCTRL
        "shift" => 42,              // KEY_LEFTSHIFT
        "alt" | "option" => 56,     // KEY_LEFTALT
        "meta" | "super" | "cmd" | "command" | "win" => 125, // KEY_LEFTMETA
        _ => return None,
    })
}

/// (keycode, needs_shift) for a printable ASCII char — for typing text via
/// per-key events on the direct backend.
pub fn char_keycode(c: char) -> Option<(u16, bool)> {
    Some(match c {
        'a'..='z' => (LETTER_ROW[(c as u8 - b'a') as usize], false),
        'A'..='Z' => (LETTER_ROW[(c.to_ascii_lowercase() as u8 - b'a') as usize], true),
        '1' => (2, false), '2' => (3, false), '3' => (4, false), '4' => (5, false),
        '5' => (6, false), '6' => (7, false), '7' => (8, false), '8' => (9, false),
        '9' => (10, false), '0' => (11, false),
        '!' => (2, true), '@' => (3, true), '#' => (4, true), '$' => (5, true),
        '%' => (6, true), '^' => (7, true), '&' => (8, true), '*' => (9, true),
        '(' => (10, true), ')' => (11, true),
        '-' => (12, false), '_' => (12, true),
        '=' => (13, false), '+' => (13, true),
        '[' => (26, false), '{' => (26, true),
        ']' => (27, false), '}' => (27, true),
        '\\' => (43, false), '|' => (43, true),
        ';' => (39, false), ':' => (39, true),
        '\'' => (40, false), '"' => (40, true),
        '`' => (41, false), '~' => (41, true),
        ',' => (51, false), '<' => (51, true),
        '.' => (52, false), '>' => (52, true),
        '/' => (53, false), '?' => (53, true),
        ' ' => (57, false),
        '\t' => (15, false),
        '\n' => (28, false),
        _ => return None,
    })
}

/// KEY_A..KEY_Z are not contiguous in evdev (they follow the QWERTY row
/// order), so map a..z explicitly.
const LETTER_ROW: [u16; 26] = [
    30, // a
    48, // b
    46, // c
    32, // d
    18, // e
    33, // f
    34, // g
    35, // h
    23, // i
    36, // j
    37, // k
    38, // l
    50, // m
    49, // n
    24, // o
    25, // p
    16, // q
    19, // r
    31, // s
    20, // t
    22, // u
    47, // v
    17, // w
    45, // x
    21, // y
    44, // z
];

fn single_char(s: &str) -> Option<char> {
    let mut it = s.chars();
    match (it.next(), it.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

/// KEY_F1=59..KEY_F10=68, then KEY_F11=87, KEY_F12=88.
fn function_key(n: &str) -> Option<u16> {
    let num: u16 = n.strip_prefix('f')?.parse().ok()?;
    match num {
        1..=10 => Some(58 + num),
        11 => Some(87),
        12 => Some(88),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_keys_map_to_evdev() {
        assert_eq!(keycode("enter"), Some(28));
        assert_eq!(keycode("Escape"), Some(1));
        assert_eq!(keycode("left"), Some(105));
        assert_eq!(keycode("F5"), Some(63));
        assert_eq!(keycode("f12"), Some(88));
    }

    #[test]
    fn letters_and_shifted_symbols() {
        assert_eq!(keycode("a"), Some(30));
        assert_eq!(keycode("z"), Some(44));
        assert_eq!(char_keycode('A'), Some((30, true)));
        assert_eq!(char_keycode('!'), Some((2, true)));
        assert_eq!(char_keycode('5'), Some((6, false)));
    }

    #[test]
    fn modifiers_are_left_variants() {
        assert_eq!(modifier_keycode("ctrl"), Some(29));
        assert_eq!(modifier_keycode("super"), Some(125));
        assert_eq!(modifier_keycode("nope"), None);
    }

    #[test]
    fn unknown_returns_none() {
        assert_eq!(keycode("nonsense-key"), None);
        assert_eq!(char_keycode('€'), None);
    }
}
