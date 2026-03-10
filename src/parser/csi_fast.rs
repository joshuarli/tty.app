use crate::parser::perform::Perform;

/// Optimistic CSI fast-path parser.
/// Handles the ~15 most common CSI sequences inline without the full state machine.
///
/// Input buffer starts AFTER "ESC [" has been consumed.
/// Returns Some(bytes_consumed) on success, None if sequence is unrecognized
/// (caller should fall back to the state machine).
pub struct CsiFastParser;

impl CsiFastParser {
    /// Ultra-fast inline SGR for the ~90% of SGR sequences that are 1-2 params
    /// with 1-2 digits each. Avoids the generic parameter parsing loop entirely.
    ///
    /// Input starts AFTER "ESC [". Returns Some(bytes_consumed) on success.
    #[inline(always)]
    pub fn try_sgr_fast<P: Perform>(buf: &[u8], performer: &mut P) -> Option<usize> {
        if buf.len() < 2 {
            return None;
        }

        let b0 = buf[0];
        let b1 = buf[1];

        // \x1b[Nm — single digit (reset, bold, italic, underline, inverse, etc.)
        if b0.is_ascii_digit() && b1 == b'm' {
            performer.sgr(&[(b0 - b'0') as u16]);
            return Some(2);
        }

        if buf.len() < 3 {
            return None;
        }
        let b2 = buf[2];

        // \x1b[NNm — two digit (fg 30-37, bg 40-47, etc.)
        if b0.is_ascii_digit() && b1.is_ascii_digit() && b2 == b'm' {
            performer.sgr(&[((b0 - b'0') * 10 + (b1 - b'0')) as u16]);
            return Some(3);
        }

        // \x1b[N;... — single digit first param + semicolon
        if b0.is_ascii_digit() && b1 == b';' && b2.is_ascii_digit() {
            if buf.len() >= 4 && buf[3] == b'm' {
                // \x1b[N;Mm
                performer.sgr(&[(b0 - b'0') as u16, (b2 - b'0') as u16]);
                return Some(4);
            }
            if buf.len() >= 5 && buf[3].is_ascii_digit() && buf[4] == b'm' {
                // \x1b[N;NNm (e.g., \x1b[1;32m bold green)
                performer.sgr(&[
                    (b0 - b'0') as u16,
                    ((b2 - b'0') * 10 + (buf[3] - b'0')) as u16,
                ]);
                return Some(5);
            }
        }

        // \x1b[NN;... — two digit first param + semicolon
        if buf.len() >= 5 && b0.is_ascii_digit() && b1.is_ascii_digit() && b2 == b';' {
            let p0 = ((b0 - b'0') * 10 + (b1 - b'0')) as u16;
            let b3 = buf[3];
            if b3.is_ascii_digit() {
                if buf[4] == b'm' {
                    // \x1b[NN;Nm (e.g., \x1b[41;7m)
                    performer.sgr(&[p0, (b3 - b'0') as u16]);
                    return Some(5);
                }
                if buf.len() >= 6 && buf[4].is_ascii_digit() && buf[5] == b'm' {
                    // \x1b[NN;NNm (e.g., \x1b[41;37m)
                    performer.sgr(&[p0, ((b3 - b'0') * 10 + (buf[4] - b'0')) as u16]);
                    return Some(6);
                }
            }
        }

        None
    }

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
            if pos >= len {
                return None;
            }
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
        let param_start = pos;

        while pos < len {
            let b = buf[pos];
            match b {
                b'0'..=b'9' => {
                    current_param = current_param
                        .saturating_mul(10)
                        .saturating_add((b - b'0') as u32);
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
                    // Colon sub-parameter — treat like ';' for flat params,
                    // but flag so SGR can reparse the raw bytes with colon awareness
                    has_colon = true;
                    if param_count < 16 {
                        params[param_count] = current_param.min(u16::MAX as u32) as u16;
                        param_count += 1;
                    }
                    current_param = 0;
                    has_digit = false;
                    pos += 1;
                }
                b' '..=b'/' => {
                    // Intermediate bytes — bail to state machine
                    return None;
                }
                0x40..=0x7E => {
                    // Final byte — dispatch
                    if (has_digit || param_count > 0) && param_count < 16 {
                        params[param_count] = current_param.min(u16::MAX as u32) as u16;
                        param_count += 1;
                    }
                    let param_end = pos;
                    pos += 1; // consume final byte

                    if has_colon && b == b'm' && !private {
                        // SGR with colon sub-params — parse raw bytes for proper grouping
                        performer.sgr_colon(&buf[param_start..param_end]);
                    } else {
                        Self::dispatch(b, &params[..param_count], private, performer);
                    }
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
                    // Unknown private CSI — pass '?' as intermediate so csi_dispatch
                    // can distinguish CSI ? Ps X from CSI Ps X
                    performer.csi_dispatch(params, b"?", false, final_byte);
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
            b'E' => {
                // CNL — cursor next line
                performer.cursor_down(p0.max(1));
                performer.cursor_horizontal_absolute(1);
            }
            b'F' => {
                // CPL — cursor previous line
                performer.cursor_up(p0.max(1));
                performer.cursor_horizontal_absolute(1);
            }

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

            // Erase characters (ECH)
            b'X' => performer.erase_chars(p0.max(1)),

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

            // REP — repeat last character
            b'b' => performer.repeat_char(p0.max(1)),

            // Save/restore cursor (ANSI.SYS-style)
            b's' => performer.save_cursor(),
            b'u' => performer.restore_cursor(),

            _ => {
                performer.csi_dispatch(params, &[], false, final_byte);
            }
        }
    }
}
