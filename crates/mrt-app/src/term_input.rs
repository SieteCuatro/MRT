//! Turns egui input events into the bytes a terminal program expects (xterm
//! conventions), taking the program's terminal modes into account: application
//! cursor keys (DECCKM) and bracketed paste (mode 2004).
//!
//! Keys bound to MRT actions never get here: `handle_keybindings` removes them
//! from the input first.

use egui::{Event, Key, Modifiers};
use mrt_core::TermModes;

/// Bytes to write to the PTY for one frame's input events, in order.
///
/// Some platforms (Windows) report Alt+letter both as a key event and as the plain
/// letter in a text event right after it. The key event is sent as `ESC letter`, so
/// that text event is dropped instead of sending the letter twice.
pub fn encode_events(events: &[Event], modes: &TermModes) -> Vec<u8> {
    let mut out = Vec::new();
    let mut skip_text: Option<char> = None;
    for event in events {
        if let Event::Text(text) = event {
            let mut chars = text.chars();
            if let (Some(skip), Some(c), None) = (skip_text.take(), chars.next(), chars.next()) {
                if c.eq_ignore_ascii_case(&skip) {
                    continue;
                }
            }
        }
        let Some(bytes) = encode_event(event, modes) else {
            continue;
        };
        if let Event::Key { modifiers, .. } = event {
            if modifiers.alt && !modifiers.ctrl && bytes.len() == 2 && bytes[0] == 0x1b {
                skip_text = Some(bytes[1] as char);
            }
        }
        out.extend(bytes);
    }
    out
}

/// Bytes to write to the PTY for `event`, or `None` when the event sends nothing.
pub fn encode_event(event: &Event, modes: &TermModes) -> Option<Vec<u8>> {
    match event {
        Event::Text(text) if !text.is_empty() => Some(text.as_bytes().to_vec()),
        Event::Paste(text) if !text.is_empty() => Some(encode_paste(text, modes)),
        // egui-winit reports Ctrl+C and Ctrl+X only as Copy/Cut, never as key events.
        // Outside macOS (where they come from Cmd+C/X) they are the terminal's ^C and ^X;
        // the overlay handles Copy itself when it copies a selection.
        Event::Copy if !cfg!(target_os = "macos") => Some(vec![0x03]),
        Event::Cut if !cfg!(target_os = "macos") => Some(vec![0x18]),
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

    // Alt+letter or digit: ESC followed by the character (the "meta sends escape"
    // convention used by readline, emacs and most TUIs). Ctrl+Alt is AltGr on many
    // keyboard layouts and produces text instead, so it is left alone.
    if modifiers.alt && !ctrl {
        let name = key.name();
        if name.len() == 1 && name.as_bytes()[0].is_ascii_alphanumeric() {
            let c = name.as_bytes()[0];
            let c = if modifiers.shift { c.to_ascii_uppercase() } else { c.to_ascii_lowercase() };
            return Some(vec![0x1b, c]);
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

/// A mouse button the terminal reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

impl MouseButton {
    pub fn from_egui(button: egui::PointerButton) -> Option<Self> {
        match button {
            egui::PointerButton::Primary => Some(Self::Left),
            egui::PointerButton::Middle => Some(Self::Middle),
            egui::PointerButton::Secondary => Some(Self::Right),
            _ => None,
        }
    }

    fn code(self) -> u8 {
        match self {
            Self::Left => 0,
            Self::Middle => 1,
            Self::Right => 2,
        }
    }
}

/// What happened with the mouse over the terminal grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    Press(MouseButton),
    Release(MouseButton),
    /// The pointer moved to another cell, with this button held (or none).
    Move(Option<MouseButton>),
    WheelUp,
    WheelDown,
}

/// The report for a mouse action at 0-based `col`/`row`, if the program asked for
/// mouse events of that kind (modes 1000/1002/1003). Uses the SGR encoding (1006)
/// when enabled, otherwise the classic X10 encoding, which cannot report columns or
/// rows past 223.
pub fn encode_mouse(action: MouseAction, col: u16, row: u16, modifiers: Modifiers, modes: &TermModes) -> Option<Vec<u8>> {
    use mrt_core::MouseMode;
    let wanted = match (action, modes.mouse) {
        (_, MouseMode::None) => false,
        (MouseAction::Move(None), mode) => mode == MouseMode::Motion,
        (MouseAction::Move(Some(_)), mode) => mode == MouseMode::Drag || mode == MouseMode::Motion,
        _ => true,
    };
    if !wanted {
        return None;
    }
    let mut code = match action {
        MouseAction::Press(b) | MouseAction::Release(b) => b.code(),
        MouseAction::Move(held) => 32 + held.map_or(3, MouseButton::code),
        MouseAction::WheelUp => 64,
        MouseAction::WheelDown => 65,
    };
    code += 4 * u8::from(modifiers.shift) + 8 * u8::from(modifiers.alt) + 16 * u8::from(modifiers.ctrl);
    let release = matches!(action, MouseAction::Release(_));
    if modes.sgr_mouse {
        let end = if release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{};{}{end}", col as u32 + 1, row as u32 + 1).into_bytes());
    }
    // X10: a release does not say which button; every value is offset by 32.
    if release {
        code = (code & !3) | 3;
    }
    let (x, y) = (col as u32 + 33, row as u32 + 33);
    if x > 255 || y > 255 {
        return None;
    }
    Some(vec![0x1b, b'[', b'M', code + 32, x as u8, y as u8])
}

/// When the program does not take mouse events but shows the alternate screen with
/// mode 1007 (alternate scroll), the wheel scrolls it with arrow keys, as in xterm.
pub fn wheel_as_arrows(up: bool, modes: &TermModes) -> Option<Vec<u8>> {
    if modes.mouse != mrt_core::MouseMode::None || !(modes.alt_screen && modes.alternate_scroll) {
        return None;
    }
    let key = if up { Key::ArrowUp } else { Key::ArrowDown };
    let one = encode_key(key, Modifiers::NONE, modes)?;
    Some(one.repeat(3))
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
    fn alt_letters_send_escape_prefix() {
        let m = TermModes::default();
        let alt = Modifiers {
            alt: true,
            ..Modifiers::NONE
        };
        assert_eq!(key(Key::B, alt, &m), "\x1bb");
        assert_eq!(key(Key::Num3, alt, &m), "\x1b3");
        let alt_shift = Modifiers { shift: true, ..alt };
        assert_eq!(key(Key::F, alt_shift, &m), "\x1bF");
        // Ctrl+Alt (AltGr) does not produce an escape sequence.
        let altgr = Modifiers { ctrl: true, ..alt };
        assert!(encode_key(Key::Q, altgr, &m).map_or(true, |b| b != b"\x1bq"));
    }

    fn key_event(key: Key, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn alt_letter_is_not_sent_twice_when_text_follows() {
        let m = TermModes::default();
        let alt = Modifiers {
            alt: true,
            ..Modifiers::NONE
        };
        // Windows: the key event and then the plain letter as text.
        let events = [key_event(Key::B, alt), Event::Text("b".into()), Event::Text("x".into())];
        assert_eq!(encode_events(&events, &m), b"\x1bbx");
        // Without the duplicate text event the result is the same.
        let events = [key_event(Key::B, alt), Event::Text("x".into())];
        assert_eq!(encode_events(&events, &m), b"\x1bbx");
        // A plain `b` typed later is still sent.
        let events = [key_event(Key::B, alt), Event::Text("x".into()), Event::Text("b".into())];
        assert_eq!(encode_events(&events, &m), b"\x1bbxb");
    }

    fn mouse_modes(mode: mrt_core::MouseMode, sgr: bool) -> TermModes {
        TermModes {
            mouse: mode,
            sgr_mouse: sgr,
            ..TermModes::default()
        }
    }

    #[test]
    fn mouse_reports_follow_the_requested_mode() {
        use mrt_core::MouseMode;
        let none = Modifiers::NONE;
        // No mouse mode: nothing is reported.
        assert!(encode_mouse(MouseAction::Press(MouseButton::Left), 0, 0, none, &TermModes::default()).is_none());

        let click = mouse_modes(MouseMode::Click, true);
        assert_eq!(encode_mouse(MouseAction::Press(MouseButton::Left), 4, 9, none, &click).unwrap(), b"\x1b[<0;5;10M");
        assert_eq!(encode_mouse(MouseAction::Release(MouseButton::Left), 4, 9, none, &click).unwrap(), b"\x1b[<0;5;10m");
        assert_eq!(encode_mouse(MouseAction::WheelUp, 0, 0, none, &click).unwrap(), b"\x1b[<64;1;1M");
        // Click mode ignores motion; drag mode reports motion with a button held.
        assert!(encode_mouse(MouseAction::Move(Some(MouseButton::Left)), 1, 1, none, &click).is_none());
        let drag = mouse_modes(MouseMode::Drag, true);
        assert_eq!(encode_mouse(MouseAction::Move(Some(MouseButton::Left)), 1, 1, none, &drag).unwrap(), b"\x1b[<32;2;2M");
        assert!(encode_mouse(MouseAction::Move(None), 1, 1, none, &drag).is_none());
        let motion = mouse_modes(MouseMode::Motion, true);
        assert_eq!(encode_mouse(MouseAction::Move(None), 1, 1, none, &motion).unwrap(), b"\x1b[<35;2;2M");
        // Modifiers add 4 (shift), 8 (alt), 16 (ctrl).
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        };
        assert_eq!(encode_mouse(MouseAction::Press(MouseButton::Right), 0, 0, ctrl, &click).unwrap(), b"\x1b[<18;1;1M");
    }

    #[test]
    fn x10_mouse_encoding_and_its_limits() {
        let x10 = mouse_modes(mrt_core::MouseMode::Click, false);
        let none = Modifiers::NONE;
        assert_eq!(encode_mouse(MouseAction::Press(MouseButton::Left), 0, 0, none, &x10).unwrap(), vec![0x1b, b'[', b'M', 32, 33, 33]);
        // Releases report button 3.
        assert_eq!(encode_mouse(MouseAction::Release(MouseButton::Right), 2, 1, none, &x10).unwrap(), vec![0x1b, b'[', b'M', 35, 35, 34]);
        // Past column 223 X10 cannot encode the position.
        assert!(encode_mouse(MouseAction::Press(MouseButton::Left), 230, 0, none, &x10).is_none());
    }

    #[test]
    fn wheel_scrolls_alternate_screen_with_arrows() {
        let alt_scroll = TermModes {
            alt_screen: true,
            alternate_scroll: true,
            ..TermModes::default()
        };
        assert_eq!(wheel_as_arrows(true, &alt_scroll).unwrap(), b"\x1b[A\x1b[A\x1b[A");
        assert!(wheel_as_arrows(true, &TermModes::default()).is_none());
        let with_mouse = TermModes {
            mouse: mrt_core::MouseMode::Click,
            ..alt_scroll
        };
        assert!(wheel_as_arrows(false, &with_mouse).is_none());
    }

    #[test]
    fn copy_and_cut_events_are_ctrl_c_and_ctrl_x_outside_macos() {
        let m = TermModes::default();
        let expected_c: Option<Vec<u8>> = (!cfg!(target_os = "macos")).then(|| vec![0x03]);
        let expected_x: Option<Vec<u8>> = (!cfg!(target_os = "macos")).then(|| vec![0x18]);
        assert_eq!(encode_event(&Event::Copy, &m), expected_c);
        assert_eq!(encode_event(&Event::Cut, &m), expected_x);
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
