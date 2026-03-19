use crate::parser::perform::Perform;

/// Optimistic CSI fast-path parser.
/// Handles the ~15 most common CSI sequences inline without the full state machine.
///
/// Input buffer starts AFTER "ESC [" has been consumed.
/// Returns Some(bytes_consumed) on success, None if sequence is unrecognized
/// (caller should fall back to the state machine).
pub struct CsiFastParser;

impl CsiFastParser {
    /// Ultra-fast inline matching for the most common CSI sequences:
    /// SGR (1-2 params, 256-color, truecolor), CUP, and EL.
    /// Avoids the generic parameter parsing loop entirely.
    ///
    /// Input starts AFTER "ESC [". Returns Some(bytes_consumed) on success.
    #[inline(always)]
    pub fn try_csi_inline<P: Perform>(buf: &[u8], performer: &mut P) -> Option<usize> {
        if buf.is_empty() {
            return None;
        }

        let b0 = buf[0];

        // Digit-first branch: the common case for SGR, CUP, 256-color, truecolor.
        // Checking digits first avoids branch mispredictions in color-heavy output
        // where most CSI sequences start with a digit.
        if b0.is_ascii_digit() {
            if buf.len() < 2 {
                return None;
            }
            let b1 = buf[1];

            // \x1b[0m — SGR reset (the single most common CSI sequence)
            if b0 == b'0' && b1 == b'm' {
                performer.sgr_reset();
                return Some(2);
            }

            // \x1b[Nm — single digit SGR
            if b1 == b'm' {
                performer.sgr_single((b0 - b'0') as u16);
                return Some(2);
            }

            // \x1b[NK — erase in line with mode
            if b1 == b'K' {
                performer.erase_in_line((b0 - b'0') as u16);
                return Some(2);
            }

            // \x1b[NJ — erase in display with mode
            if b1 == b'J' {
                performer.erase_in_display((b0 - b'0') as u16);
                return Some(2);
            }

            if buf.len() < 3 {
                return None;
            }
            let b2 = buf[2];

            // \x1b[NNm — two digit SGR (fg 30-37, bg 40-47, etc.)
            if b1.is_ascii_digit() && b2 == b'm' {
                performer.sgr_single(((b0 - b'0') * 10 + (b1 - b'0')) as u16);
                return Some(3);
            }

            // \x1b[N;... — single digit first param + semicolon
            if b1 == b';' && b2.is_ascii_digit() {
                if buf.len() >= 4 {
                    let final_byte = buf[3];
                    if final_byte == b'm' {
                        // \x1b[N;Mm — compound SGR (e.g., \x1b[1;7m bold+inverse)
                        performer.sgr_single((b0 - b'0') as u16);
                        performer.sgr_single((b2 - b'0') as u16);
                        return Some(4);
                    }
                    if final_byte == b'H' {
                        performer.cursor_position((b0 - b'0') as u16, (b2 - b'0') as u16);
                        return Some(4);
                    }
                }
                if buf.len() >= 5 && buf[3].is_ascii_digit() {
                    let p1 = ((b2 - b'0') * 10 + (buf[3] - b'0')) as u16;
                    let final_byte = buf[4];
                    if final_byte == b'm' {
                        // \x1b[N;NNm — compound SGR (e.g., \x1b[1;32m bold+green)
                        performer.sgr_single((b0 - b'0') as u16);
                        performer.sgr_single(p1);
                        return Some(5);
                    }
                    if final_byte == b'H' {
                        performer.cursor_position((b0 - b'0') as u16, p1);
                        return Some(5);
                    }
                }
            }

            // \x1b[NN;... — two digit first param + semicolon
            if buf.len() >= 5 && b1.is_ascii_digit() && b2 == b';' {
                let p0 = ((b0 - b'0') * 10 + (b1 - b'0')) as u16;
                let b3 = buf[3];
                if b3.is_ascii_digit() {
                    if buf[4] == b'm' {
                        // \x1b[NN;Nm — compound SGR (e.g., \x1b[41;7m)
                        performer.sgr_single(p0);
                        performer.sgr_single((b3 - b'0') as u16);
                        return Some(5);
                    }
                    if buf[4] == b'H' {
                        performer.cursor_position(p0, (b3 - b'0') as u16);
                        return Some(5);
                    }
                    if buf.len() >= 6 && buf[4].is_ascii_digit() {
                        let p1 = ((b3 - b'0') * 10 + (buf[4] - b'0')) as u16;
                        if buf[5] == b'm' {
                            // \x1b[NN;NNm — compound SGR (e.g., \x1b[41;37m)
                            performer.sgr_single(p0);
                            performer.sgr_single(p1);
                            return Some(6);
                        }
                        if buf[5] == b'H' {
                            performer.cursor_position(p0, p1);
                            return Some(6);
                        }
                        if buf.len() >= 7 && buf[5].is_ascii_digit() && buf[6] == b'H' {
                            let p1 = p1 * 10 + (buf[5] - b'0') as u16;
                            performer.cursor_position(p0, p1);
                            return Some(7);
                        }
                    }
                }

                // 256-color: \x1b[38;5;Nm or \x1b[48;5;Nm
                if (p0 == 38 || p0 == 48)
                    && b3 == b'5'
                    && buf[4] == b';'
                    && let Some(consumed) = Self::parse_color_index(&buf[5..])
                {
                    performer.color_256(p0 == 38, consumed.0);
                    return Some(5 + consumed.1);
                }

                // Truecolor: \x1b[38;2;R;G;Bm or \x1b[48;2;R;G;Bm
                if (p0 == 38 || p0 == 48)
                    && b3 == b'2'
                    && buf[4] == b';'
                    && let Some((r, g, b, consumed)) = Self::parse_rgb(&buf[5..])
                {
                    performer.color_rgb(p0 == 38, r, g, b);
                    return Some(5 + consumed);
                }
            }

            return None;
        }

        // Non-digit leading byte: K, H (the common non-digit CSI sequences)
        if b0 == b'K' {
            performer.erase_in_line(0);
            return Some(1);
        }
        if b0 == b'H' {
            performer.cursor_position(1, 1);
            return Some(1);
        }

        None
    }

    /// Parse a 1-3 digit number followed by 'm'. Returns (value, bytes_consumed).
    #[inline(always)]
    fn parse_color_index(buf: &[u8]) -> Option<(u16, usize)> {
        if buf.is_empty() || !buf[0].is_ascii_digit() {
            return None;
        }
        let mut val = (buf[0] - b'0') as u16;
        let mut i = 1;
        while i < buf.len() && i < 3 && buf[i].is_ascii_digit() {
            val = val * 10 + (buf[i] - b'0') as u16;
            i += 1;
        }
        if i < buf.len() && buf[i] == b'm' {
            Some((val, i + 1))
        } else {
            None
        }
    }

    /// Parse R;G;Bm (three 1-3 digit numbers separated by ';' ending with 'm').
    /// Returns (r, g, b, bytes_consumed).
    #[inline(always)]
    fn parse_rgb(buf: &[u8]) -> Option<(u16, u16, u16, usize)> {
        let mut pos = 0;
        let mut components = [0u16; 3];
        for (c, component) in components.iter_mut().enumerate() {
            if pos >= buf.len() || !buf[pos].is_ascii_digit() {
                return None;
            }
            let mut val = (buf[pos] - b'0') as u16;
            pos += 1;
            while pos < buf.len() && buf[pos].is_ascii_digit() && val < 1000 {
                val = val * 10 + (buf[pos] - b'0') as u16;
                pos += 1;
            }
            *component = val;
            if c < 2 {
                if pos >= buf.len() || buf[pos] != b';' {
                    return None;
                }
                pos += 1; // consume ';'
            }
        }
        if pos < buf.len() && buf[pos] == b'm' {
            Some((components[0], components[1], components[2], pos + 1))
        } else {
            None
        }
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

    /// Dispatch a parsed CSI sequence to Perform trait methods.
    ///
    /// Handles all standard (no-intermediate) CSI sequences. Also used as the
    /// shared dispatch table for split sequences that go through the state
    /// machine → csi_dispatch fallback path.
    pub fn dispatch<P: Perform>(final_byte: u8, params: &[u16], private: bool, performer: &mut P) {
        let p0 = params.first().copied().unwrap_or(0);
        let p1 = if params.len() > 1 { params[1] } else { 0 };

        if private {
            match final_byte {
                b'h' => performer.set_mode(params, true),
                b'l' => performer.reset_mode(params, true),
                _ => {}
            }
            return;
        }

        match final_byte {
            // SGR (Select Graphic Rendition)
            b'm' => {
                if params.is_empty() || (params.len() == 1 && params[0] == 0) {
                    performer.sgr_reset();
                } else if params.len() == 1 {
                    performer.sgr_single(params[0]);
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

            _ => {}
        }
    }
}
