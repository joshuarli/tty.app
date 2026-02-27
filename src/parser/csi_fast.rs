use crate::parser::perform::Perform;

/// Optimistic CSI fast-path parser.
/// Handles the ~15 most common CSI sequences inline without the full state machine.
///
/// Input buffer starts AFTER "ESC [" has been consumed.
/// Returns Some(bytes_consumed) on success, None if sequence is unrecognized
/// (caller should fall back to the state machine).
pub struct CsiFastParser;

impl CsiFastParser {
    /// Try to parse a CSI sequence starting after "ESC [".
    /// Returns Some(consumed_bytes) on success, None to bail to state machine.
    pub fn try_parse<P: Perform>(buf: &[u8], performer: &mut P) -> Option<usize> {
        if buf.is_empty() {
            return None;
        }

        let mut pos = 0;
        let len = buf.len();

        // Check for private mode prefix '?'
        let private = if buf[pos] == b'?' {
            pos += 1;
            if pos >= len { return None; }
            true
        } else {
            false
        };

        // Parse parameters (semicolon-separated numbers)
        let mut params = [0u16; 16];
        let mut param_count = 0;
        let mut current_param: u32 = 0;
        let mut has_digit = false;
        let mut has_colon = false;

        while pos < len {
            let b = buf[pos];
            match b {
                b'0'..=b'9' => {
                    current_param = current_param * 10 + (b - b'0') as u32;
                    has_digit = true;
                    pos += 1;
                }
                b';' => {
                    if param_count < 16 {
                        params[param_count] = current_param.min(u16::MAX as u32) as u16;
                        param_count += 1;
                    }
                    current_param = 0;
                    has_digit = false;
                    pos += 1;
                }
                b':' => {
                    // Colon sub-parameters — bail to state machine
                    has_colon = true;
                    return None;
                }
                b' '..=b'/' => {
                    // Intermediate bytes — bail to state machine
                    return None;
                }
                0x40..=0x7E => {
                    // Final byte — dispatch
                    if has_digit || param_count > 0 {
                        if param_count < 16 {
                            params[param_count] = current_param.min(u16::MAX as u32) as u16;
                            param_count += 1;
                        }
                    }
                    pos += 1; // consume final byte

                    Self::dispatch(b, &params[..param_count], private, performer);
                    return Some(pos);
                }
                _ => {
                    // Unexpected byte — bail
                    return None;
                }
            }
        }

        // Ran out of buffer without finding final byte
        None
    }

    fn dispatch<P: Perform>(final_byte: u8, params: &[u16], private: bool, performer: &mut P) {
        let p0 = params.first().copied().unwrap_or(0);
        let p1 = if params.len() > 1 { params[1] } else { 0 };

        if private {
            match final_byte {
                b'h' => performer.set_mode(params, true),
                b'l' => performer.reset_mode(params, true),
                _ => {
                    // Unknown private CSI — dispatch generically
                    performer.csi_dispatch(params, &[], false, final_byte);
                }
            }
            return;
        }

        match final_byte {
            // SGR (Select Graphic Rendition)
            b'm' => {
                if params.is_empty() {
                    performer.sgr(&[0]);
                } else {
                    performer.sgr(params);
                }
            }

            // Cursor movement
            b'A' => performer.cursor_up(p0.max(1)),
            b'B' | b'e' => performer.cursor_down(p0.max(1)),
            b'C' | b'a' => performer.cursor_forward(p0.max(1)),
            b'D' => performer.cursor_backward(p0.max(1)),

            // Cursor position
            b'H' | b'f' => {
                let row = p0.max(1);
                let col = if params.len() > 1 { p1.max(1) } else { 1 };
                performer.cursor_position(row, col);
            }
            b'G' | b'`' => performer.cursor_horizontal_absolute(p0.max(1)),
            b'd' => performer.cursor_vertical_absolute(p0.max(1)),

            // Erase
            b'J' => performer.erase_in_display(p0),
            b'K' => performer.erase_in_line(p0),

            // Scroll
            b'S' => performer.scroll_up(p0.max(1)),
            b'T' => performer.scroll_down(p0.max(1)),

            // Insert/delete
            b'L' => performer.insert_lines(p0.max(1)),
            b'M' => performer.delete_lines(p0.max(1)),
            b'@' => performer.insert_chars(p0.max(1)),
            b'P' => performer.delete_chars(p0.max(1)),

            // Erase characters
            b'X' => {
                // ECH — erase n characters from cursor
                performer.insert_chars(0); // reuse for now, handled in csi_dispatch
                performer.csi_dispatch(params, &[], false, final_byte);
            }

            // Set/reset mode (non-private)
            b'h' => performer.set_mode(params, false),
            b'l' => performer.reset_mode(params, false),

            // Set scroll region (DECSTBM)
            b'r' => {
                let top = p0.max(1);
                let bottom = if params.len() > 1 && p1 > 0 { p1 } else { 0 }; // 0 = use max rows
                performer.set_scroll_region(top, bottom);
            }

            // Tab clear
            b'g' => performer.tab_clear(p0),

            // Device status report
            b'n' => performer.device_status_report(p0),

            // Cursor style (DECSCUSR) — CSI Ps SP q
            // Note: this has an intermediate space, so it won't hit this path
            // (the space causes a bail to state machine). That's fine.
            b'q' => performer.set_cursor_style(p0),

            // Save/restore cursor (ANSI.SYS-style)
            b's' => performer.save_cursor(),
            b'u' => performer.restore_cursor(),

            _ => {
                performer.csi_dispatch(params, &[], false, final_byte);
            }
        }
    }
}
