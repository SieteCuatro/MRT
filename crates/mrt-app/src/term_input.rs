//! Turns egui input events into the bytes a terminal program expects (xterm
//! conventions), taking the program's terminal modes into account: application
//! cursor keys (DECCKM) and bracketed paste (mode 2004).
//!
//! Keys bound to MRT actions never get here: `handle_keybindings` removes them
//! from the input first.

use egui::{Event, Key, Modifiers};
use mrt_core::TermModes;

/// Bytes to write to the PTY for `event`, or `None` when the event sends nothing.
pub fn encode_event(event: &Event, modes: &TermModes) -> Option<Vec<u8>> {
    match event {
        Event::Text(text) if !text.is_empty() => Some(text.as_bytes().to_vec()),
        Event::Paste(text) if !text.is_empty() => Some(encode_paste(text, modes)),
        Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } => encode_key(*key, *modifiers, modes),
        _ => None,
    }
}

/// Pasted text, normalised to `\r` line endings (what a terminal sends for Enter)
/// and wrapped in `ESC [200~` / `ESC [201~` when the program enabled bracketed paste.
pub fn encode_paste(text: &str, modes: &TermModes) -> Vec<u8> {
    let normalised = text.replace("\r\n", "\r").replace('\n', "\r");
    if modes.bracketed_paste {
        // A paste must not be able to end the bracket early.
        let inner = normalised.replace("\x1b[201~", "");
        format!("\x1b[200~{inner}\x1b[201~").into_bytes()
    } else {
        normalised.into_bytes()
    }
}

/// Bytes for a key press that egui does not also report as `Event::Text`.
pub fn encode_key(key: Key, modifiers: Modifiers, modes: &TermModes) -> Option<Vec<u8>> {
    // The physical Ctrl key only: on macOS, Cmd shortcuts belong to MRT, as in other terminals.
    let ctrl = modifiers.ctrl;
    if ctrl && !modifiers.alt {
        if let Some(byte) = ctrl_byte(key) {
            return Some(vec![byte]);
        }
    }

    // xterm modifier parameter: 1 + shift(1) + alt(2) + ctrl(4).
    let modifier_param = 1 + u8::from(modifiers.shift) + 2 * u8::from(modifiers.alt) + 4 * u8::from(ctrl);

    let seq: String = match key {
        Key::Enter => "\r".into(),
        Key::Tab if modifiers.shift => "\x1b[Z".into(),
        Key::Tab => "\t".into(),
        Key::Backspace if modifiers.alt => "\x1b\x7f".into(),
        Key::Backspace => "\x7f".into(),
        Key::Escape => "\x1b".into(),
        Key::ArrowUp => cursor_key('A', modifier_param, modes),
        Key::ArrowDown => cursor_key('B', modifier_param, modes),
        Key::ArrowRight => cursor_key('C', modifier_param, modes),
        Key::ArrowLeft => cursor_key('D', modifier_param, modes),
        Key::Home => cursor_key('H', modifier_param, modes),
        Key::End => cursor_key('F', modifier_param, modes),
        Key::Insert => tilde_key(2, modifier_param),
        Key::Delete => tilde_key(3, modifier_param),
        Key::PageUp => tilde_key(5, modifier_param),
        Key::PageDown => tilde_key(6, modifier_param),
        Key::F1 => ss3_or_csi('P', modifier_param),
        Key::F2 => ss3_or_csi('Q', modifier_param),
        Key::F3 => ss3_or_csi('R', modifier_param),
        Key::F4 => ss3_or_csi('S', modifier_param),
        Key::F5 => tilde_key(15, modifier_param),
        Key::F6 => tilde_key(17, modifier_param),
        Key::F7 => tilde_key(18, modifier_param),
        Key::F8 => tilde_key(19, modifier_param),
        Key::F9 => tilde_key(20, modifier_param),
        Key::F10 => tilde_key(21, modifier_param),
        Key::F11 => tilde_key(23, modifier_param),
        Key::F12 => tilde_key(24, modifier_param),
        _ => return None,
    };
    Some(seq.into_bytes())
}

/// Control byte for Ctrl+key: letters give 0x01-0x1a, plus the usual
/// punctuation (`Ctrl+[` is ESC, `Ctrl+Space` is NUL).
fn ctrl_byte(key: Key) -> Option<u8> {
    let name = key.name();
    if name.len() == 1 {
        let c = name.as_bytes()[0].to_ascii_uppercase();
        if c.is_ascii_uppercase() {
            return Some(c - b'A' + 1);
        }
    }
    match key {
        Key::Space => Some(0x00),
        Key::OpenBracket => Some(0x1b),
        Key::Backslash => Some(0x1c),
        Key::CloseBracket => Some(0x1d),
        _ => None,
    }
}

/// Arrow keys and Home/End: `ESC [ X` normally, `ESC O X` in application cursor
/// mode, `ESC [ 1 ; m X` with modifiers.
fn cursor_key(final_byte: char, modifier_param: u8, modes: &TermModes) -> String {
    if modifier_param > 1 {
        format!("\x1b[1;{modifier_param}{final_byte}")
    } else if modes.app_cursor {
        format!("\x1bO{final_byte}")
    } else {
        format!("\x1b[{final_byte}")
    }
}

/// Keys of the form `ESC [ n ~` (`ESC [ n ; m ~` with modifiers).
fn tilde_key(code: u8, modifier_param: u8) -> String {
    if modifier_param > 1 {
        format!("\x1b[{code};{modifier_param}~")
    } else {
        format!("\x1b[{code}~")
    }
}

/// F1-F4: `ESC O P..S`, or `ESC [ 1 ; m P..S` with modifiers.
fn ss3_or_csi(final_byte: char, modifier_param: u8) -> String {
    if modifier_param > 1 {
        format!("\x1b[1;{modifier_param}{final_byte}")
    } else {
        format!("\x1bO{final_byte}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none() -> Modifiers {
        Modifiers::NONE
    }

    fn ctrl() -> Modifiers {
        Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        }
    }

    fn key(k: Key, m: Modifiers, modes: &TermModes) -> String {
        String::from_utf8(encode_key(k, m, modes).expect("key sends bytes")).unwrap()
    }

    #[test]
    fn enter_backspace_tab_escape() {
        let m = TermModes::default();
        assert_eq!(key(Key::Enter, none(), &m), "\r");
        assert_eq!(key(Key::Backspace, none(), &m), "\x7f");
        assert_eq!(key(Key::Tab, none(), &m), "\t");
        assert_eq!(
            key(
                Key::Tab,
                Modifiers {
                    shift: true,
                    ..Modifiers::NONE
                },
                &m
            ),
            "\x1b[Z"
        );
        assert_eq!(key(Key::Escape, none(), &m), "\x1b");
    }

    #[test]
    fn arrows_follow_application_cursor_mode() {
        let normal = TermModes::default();
        let app = TermModes {
            app_cursor: true,
            ..TermModes::default()
        };
        assert_eq!(key(Key::ArrowUp, none(), &normal), "\x1b[A");
        assert_eq!(key(Key::ArrowUp, none(), &app), "\x1bOA");
        assert_eq!(key(Key::Home, none(), &app), "\x1bOH");
        // Modifiers always use the CSI form.
        assert_eq!(key(Key::ArrowLeft, ctrl(), &app), "\x1b[1;5D");
    }

    #[test]
    fn ctrl_letters_and_punctuation() {
        let m = TermModes::default();
        assert_eq!(key(Key::C, ctrl(), &m), "\x03");
        assert_eq!(key(Key::A, ctrl(), &m), "\x01");
        assert_eq!(key(Key::Z, ctrl(), &m), "\x1a");
        assert_eq!(key(Key::R, ctrl(), &m), "\x12");
        assert_eq!(key(Key::OpenBracket, ctrl(), &m), "\x1b");
        assert_eq!(key(Key::Space, ctrl(), &m), "\0");
    }

    #[test]
    fn function_and_editing_keys() {
        let m = TermModes::default();
        assert_eq!(key(Key::F1, none(), &m), "\x1bOP");
        assert_eq!(key(Key::F5, none(), &m), "\x1b[15~");
        assert_eq!(key(Key::F12, none(), &m), "\x1b[24~");
        assert_eq!(key(Key::Delete, none(), &m), "\x1b[3~");
        assert_eq!(key(Key::PageDown, none(), &m), "\x1b[6~");
        let shift = Modifiers {
            shift: true,
            ..Modifiers::NONE
        };
        assert_eq!(key(Key::F5, shift, &m), "\x1b[15;2~");
    }

    #[test]
    fn plain_letters_come_from_text_events() {
        // Without Ctrl, letters are sent through `Event::Text`, not as keys.
        assert!(encode_key(Key::A, none(), &TermModes::default()).is_none());
        let text = encode_event(&Event::Text("é".into()), &TermModes::default()).unwrap();
        assert_eq!(text, "é".as_bytes());
    }

    #[test]
    fn paste_is_bracketed_when_requested() {
        let plain = TermModes::default();
        let bracketed = TermModes {
            bracketed_paste: true,
            ..TermModes::default()
        };
        assert_eq!(encode_paste("a\nb", &plain), b"a\rb");
        assert_eq!(encode_paste("a\r\nb", &bracketed), b"\x1b[200~a\rb\x1b[201~");
        // A pasted end marker cannot close the bracket early.
        assert_eq!(encode_paste("x\x1b[201~y", &bracketed), b"\x1b[200~xy\x1b[201~");
    }
}
