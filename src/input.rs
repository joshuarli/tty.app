use crate::terminal::grid::TermMode;
use crate::window::{Key, Modifiers, NamedKey};

/// Translate a key event into VT byte sequence(s) to send to the PTY.
/// Returns None if the key should not be sent (e.g., Cmd+C for copy).
pub fn key_to_bytes(key: &Key, modifiers: &Modifiers, term_mode: TermMode) -> Option<Vec<u8>> {
    let app_cursor = term_mode.contains(TermMode::CURSOR_KEYS);

    // Cmd modifier — handle separately (Cmd+C, Cmd+V are intercepted by the caller)
    if modifiers.super_key() {
        return None;
    }

    let shift = modifiers.shift();
    let ctrl = modifiers.control();
    let alt = modifiers.alt(); // Option key = Meta = ESC prefix

    match key {
        Key::Character(s) => {
            let ch = s.chars().next()?;

            if ctrl {
                // Ctrl+letter → 0x01-0x1A
                if ch.is_ascii_lowercase() {
                    let byte = (ch as u8) - b'a' + 1;
                    return Some(maybe_esc_prefix(alt, &[byte]));
                }
                if ch.is_ascii_uppercase() {
                    let byte = (ch as u8) - b'A' + 1;
                    return Some(maybe_esc_prefix(alt, &[byte]));
                }
                // Ctrl+special characters
                match ch {
                    '@' => return Some(maybe_esc_prefix(alt, &[0x00])),
                    '[' => return Some(maybe_esc_prefix(alt, &[0x1B])),
                    '\\' => return Some(maybe_esc_prefix(alt, &[0x1C])),
                    ']' => return Some(maybe_esc_prefix(alt, &[0x1D])),
                    '^' => return Some(maybe_esc_prefix(alt, &[0x1E])),
                    '_' => return Some(maybe_esc_prefix(alt, &[0x1F])),
                    '/' => return Some(maybe_esc_prefix(alt, &[0x1F])),
                    _ => {}
                }
            }

            // Regular character — UTF-8 encode
            let mut buf = [0u8; 4];
            let s = ch.encode_utf8(&mut buf);
            Some(maybe_esc_prefix(alt, s.as_bytes()))
        }

        Key::Named(named) => {
            match named {
                // Arrow keys
                NamedKey::ArrowUp => Some(cursor_key(b'A', app_cursor, shift, alt, ctrl)),
                NamedKey::ArrowDown => Some(cursor_key(b'B', app_cursor, shift, alt, ctrl)),
                NamedKey::ArrowRight => Some(cursor_key(b'C', app_cursor, shift, alt, ctrl)),
                NamedKey::ArrowLeft => Some(cursor_key(b'D', app_cursor, shift, alt, ctrl)),

                // Home/End
                NamedKey::Home => Some(cursor_key(b'H', app_cursor, shift, alt, ctrl)),
                NamedKey::End => Some(cursor_key(b'F', app_cursor, shift, alt, ctrl)),

                // Page up/down
                NamedKey::PageUp => Some(modified_key(b"5", shift, alt, ctrl)),
                NamedKey::PageDown => Some(modified_key(b"6", shift, alt, ctrl)),

                // Insert/Delete
                NamedKey::Insert => Some(modified_key(b"2", shift, alt, ctrl)),
                NamedKey::Delete => Some(modified_key(b"3", shift, alt, ctrl)),

                // Function keys
                NamedKey::F1 => Some(maybe_esc_prefix(alt, b"\x1BOP")),
                NamedKey::F2 => Some(maybe_esc_prefix(alt, b"\x1BOQ")),
                NamedKey::F3 => Some(maybe_esc_prefix(alt, b"\x1BOR")),
                NamedKey::F4 => Some(maybe_esc_prefix(alt, b"\x1BOS")),
                NamedKey::F5 => Some(fkey(15, shift, alt, ctrl)),
                NamedKey::F6 => Some(fkey(17, shift, alt, ctrl)),
                NamedKey::F7 => Some(fkey(18, shift, alt, ctrl)),
                NamedKey::F8 => Some(fkey(19, shift, alt, ctrl)),
                NamedKey::F9 => Some(fkey(20, shift, alt, ctrl)),
                NamedKey::F10 => Some(fkey(21, shift, alt, ctrl)),
                NamedKey::F11 => Some(fkey(23, shift, alt, ctrl)),
                NamedKey::F12 => Some(fkey(24, shift, alt, ctrl)),

                // Basic keys
                NamedKey::Backspace => Some(maybe_esc_prefix(alt, &[0x7F])),
                NamedKey::Tab => {
                    if shift {
                        Some(b"\x1B[Z".to_vec()) // Back-tab
                    } else {
                        Some(maybe_esc_prefix(alt, &[0x09]))
                    }
                }
                NamedKey::Enter => Some(maybe_esc_prefix(alt, &[0x0D])),
                NamedKey::Escape => Some(vec![0x1B]),
                NamedKey::Space => {
                    if ctrl {
                        Some(maybe_esc_prefix(alt, &[0x00])) // Ctrl+Space = NUL
                    } else {
                        Some(maybe_esc_prefix(alt, &[0x20]))
                    }
                }
            }
        }
    }
}

/// Prepend ESC if Alt/Option is held (Meta key).
fn maybe_esc_prefix(alt: bool, data: &[u8]) -> Vec<u8> {
    if alt {
        let mut v = Vec::with_capacity(1 + data.len());
        v.push(0x1B);
        v.extend_from_slice(data);
        v
    } else {
        data.to_vec()
    }
}

/// Generate cursor key sequence. Handles application mode and modifiers.
fn cursor_key(key: u8, app_cursor: bool, shift: bool, alt: bool, ctrl: bool) -> Vec<u8> {
    let modifier = modifier_param(shift, alt, ctrl);

    if modifier > 1 {
        // Modified: ESC [ 1 ; <mod> <key>
        format!("\x1B[1;{}{}", modifier, key as char).into_bytes()
    } else if app_cursor {
        // Application mode: ESC O <key>
        vec![0x1B, b'O', key]
    } else {
        // Normal mode: ESC [ <key>
        vec![0x1B, b'[', key]
    }
}

/// Generate a "tilde" key sequence: ESC [ <num> ~ or ESC [ <num> ; <mod> ~
fn modified_key(num: &[u8], shift: bool, alt: bool, ctrl: bool) -> Vec<u8> {
    let modifier = modifier_param(shift, alt, ctrl);
    let mut seq = vec![0x1B, b'['];
    seq.extend_from_slice(num);
    if modifier > 1 {
        seq.push(b';');
        seq.extend_from_slice(modifier.to_string().as_bytes());
    }
    seq.push(b'~');
    seq
}

/// Generate function key sequence: ESC [ <num> ~ or ESC [ <num> ; <mod> ~
fn fkey(num: u8, shift: bool, alt: bool, ctrl: bool) -> Vec<u8> {
    let num_str = num.to_string();
    modified_key(num_str.as_bytes(), shift, alt, ctrl)
}

/// Compute the xterm modifier parameter.
/// 1 = none, 2 = shift, 3 = alt, 4 = shift+alt, etc.
fn modifier_param(shift: bool, alt: bool, ctrl: bool) -> u8 {
    let mut m = 1u8;
    if shift {
        m += 1;
    }
    if alt {
        m += 2;
    }
    if ctrl {
        m += 4;
    }
    m
}
