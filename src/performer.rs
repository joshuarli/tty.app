use std::time::Instant;

use crate::config;
use crate::parser::charset::translate_dec_special;
use crate::parser::csi_fast::CsiFastParser;
use crate::parser::perform::Perform;
use crate::renderer::atlas::Atlas;
use crate::renderer::font::FontRasterizer;
use crate::terminal::cell::{Cell, CellFlags};
use crate::terminal::grid::{Grid, TermMode};
use crate::terminal::scrollback::Scrollback;
use crate::unicode::{is_wide, is_zero_width};

/// The performer that bridges parser actions to grid mutations.
pub(crate) struct TermPerformer<'a> {
    pub(crate) grid: &'a mut Grid,
    pub(crate) scrollback: &'a mut Scrollback,
    pub(crate) atlas: &'a mut Atlas,
    pub(crate) rasterizer: &'a FontRasterizer,
    pub(crate) response_buf: &'a mut Vec<u8>,
}

impl<'a> Perform for TermPerformer<'a> {
    fn print_ascii_run(&mut self, bytes: &[u8]) {
        let use_dec = (self.grid.active_charset == 0 && self.grid.charset_g0 == 1)
            || (self.grid.active_charset == 1 && self.grid.charset_g1 == 1);

        if use_dec {
            for &b in bytes {
                let ch = if (0x60..=0x7E).contains(&b) {
                    translate_dec_special(b)
                } else {
                    b as char
                };
                let pos = self.atlas.get_or_insert(ch as u32, false, self.rasterizer);
                self.grid.write_char(ch, pos.x, pos.y);
                self.grid.last_char = ch;
                self.grid.last_atlas = [pos.x, pos.y];
            }
        } else {
            // Fast path: bulk write — atlas coords resolved inside Grid from ascii_atlas
            self.grid.write_ascii_run(bytes);
            if let Some(&last) = bytes.last() {
                self.grid.last_char = last as char;
                let ap = self.atlas.get_ascii(last);
                self.grid.last_atlas = [ap.x, ap.y];
            }
        }
    }

    fn print(&mut self, c: char) {
        let cp = c as u32;
        let wide = is_wide(cp);

        if wide {
            let pos = self.atlas.get_or_insert(cp, true, self.rasterizer);
            self.grid.write_wide_char(c, pos.x, pos.y);
            self.grid.last_char = c;
            self.grid.last_atlas = [pos.x, pos.y];
        } else if is_zero_width(cp) {
            // Zero-width combining marks — ignore for v1
        } else {
            let pos = self.atlas.get_or_insert(cp, false, self.rasterizer);
            self.grid.write_char(c, pos.x, pos.y);
            self.grid.last_char = c;
            self.grid.last_atlas = [pos.x, pos.y];
        }
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x07 => {} // BEL (TODO: visual bell)
            0x08 => self.grid.backspace(),
            0x09 => {
                // TAB
                let col = self.grid.cursor_col;
                let cols = self.grid.cols;
                let mut next = col + 1;
                while next < cols {
                    if self.grid.tab_stops[next as usize] {
                        break;
                    }
                    next += 1;
                }
                self.grid.cursor_col = next.min(cols - 1);
                self.grid.cursor_pending_wrap = false;
            }
            0x0A..=0x0C => self.grid.linefeed(Some(self.scrollback)),
            0x0D => self.grid.carriage_return(),
            0x0E => self.grid.active_charset = 1, // SO → G1
            0x0F => self.grid.active_charset = 0, // SI → G0
            _ => {}
        }
    }

    fn cursor_up(&mut self, n: u16) {
        let row = self.grid.cursor_row;
        let top = if row >= self.grid.scroll_top && row <= self.grid.scroll_bottom {
            self.grid.scroll_top
        } else {
            0
        };
        self.grid.cursor_row = row.saturating_sub(n).max(top);
        self.grid.cursor_pending_wrap = false;
        self.grid.mark_dirty(self.grid.cursor_row);
    }

    fn cursor_down(&mut self, n: u16) {
        let row = self.grid.cursor_row;
        let bottom = if row >= self.grid.scroll_top && row <= self.grid.scroll_bottom {
            self.grid.scroll_bottom
        } else {
            self.grid.rows - 1
        };
        self.grid.cursor_row = row.saturating_add(n).min(bottom);
        self.grid.cursor_pending_wrap = false;
        self.grid.mark_dirty(self.grid.cursor_row);
    }

    fn cursor_forward(&mut self, n: u16) {
        self.grid.cursor_col = self
            .grid
            .cursor_col
            .saturating_add(n)
            .min(self.grid.cols - 1);
        self.grid.cursor_pending_wrap = false;
    }

    fn cursor_backward(&mut self, n: u16) {
        self.grid.cursor_col = self.grid.cursor_col.saturating_sub(n);
        self.grid.cursor_pending_wrap = false;
    }

    fn cursor_position(&mut self, row: u16, col: u16) {
        if self.grid.mode.contains(TermMode::ORIGIN_MODE) {
            // DECOM: coordinates are relative to scroll region, clamped within it
            let top = self.grid.scroll_top;
            let bottom = self.grid.scroll_bottom;
            self.grid.cursor_row = top.saturating_add(row.saturating_sub(1)).min(bottom);
        } else {
            self.grid.cursor_row = (row.saturating_sub(1)).min(self.grid.rows - 1);
        }
        self.grid.cursor_col = (col.saturating_sub(1)).min(self.grid.cols - 1);
        self.grid.cursor_pending_wrap = false;
    }

    fn cursor_horizontal_absolute(&mut self, col: u16) {
        self.grid.cursor_col = (col.saturating_sub(1)).min(self.grid.cols - 1);
        self.grid.cursor_pending_wrap = false;
    }

    fn cursor_vertical_absolute(&mut self, row: u16) {
        if self.grid.mode.contains(TermMode::ORIGIN_MODE) {
            let top = self.grid.scroll_top;
            let bottom = self.grid.scroll_bottom;
            self.grid.cursor_row = top.saturating_add(row.saturating_sub(1)).min(bottom);
        } else {
            self.grid.cursor_row = (row.saturating_sub(1)).min(self.grid.rows - 1);
        }
        self.grid.cursor_pending_wrap = false;
    }

    fn erase_in_display(&mut self, mode: u16) {
        let row = self.grid.cursor_row;
        let col = self.grid.cursor_col;
        match mode {
            0 => {
                self.grid.clear_cols(row, col, self.grid.cols);
                self.grid.clear_rows(row + 1, self.grid.rows);
            }
            1 => {
                self.grid.clear_rows(0, row);
                self.grid.clear_cols(row, 0, col + 1);
            }
            2 => {
                self.grid.clear_rows(0, self.grid.rows);
            }
            3 => {
                self.grid.clear_rows(0, self.grid.rows);
                self.scrollback.clear();
            }
            _ => {}
        }
    }

    fn erase_in_line(&mut self, mode: u16) {
        let row = self.grid.cursor_row;
        let col = self.grid.cursor_col;
        match mode {
            0 => self.grid.clear_cols(row, col, self.grid.cols),
            1 => self.grid.clear_cols(row, 0, col + 1),
            2 => self.grid.clear_cols(row, 0, self.grid.cols),
            _ => {}
        }
    }

    fn scroll_up(&mut self, n: u16) {
        self.grid.scroll_up_into(n, Some(self.scrollback));
    }

    fn scroll_down(&mut self, n: u16) {
        self.grid.scroll_down(n);
    }

    fn insert_lines(&mut self, n: u16) {
        let row = self.grid.cursor_row;
        if row < self.grid.scroll_top || row > self.grid.scroll_bottom {
            return;
        }
        let old_top = self.grid.scroll_top;
        self.grid.scroll_top = row;
        self.grid.scroll_down(n);
        self.grid.scroll_top = old_top;
        self.grid.cursor_col = 0;
    }

    fn delete_lines(&mut self, n: u16) {
        let row = self.grid.cursor_row;
        if row < self.grid.scroll_top || row > self.grid.scroll_bottom {
            return;
        }
        let old_top = self.grid.scroll_top;
        self.grid.scroll_top = row;
        self.grid.scroll_up(n);
        self.grid.scroll_top = old_top;
        self.grid.cursor_col = 0;
    }

    fn insert_chars(&mut self, n: u16) {
        self.grid.insert_chars(n);
    }

    fn delete_chars(&mut self, n: u16) {
        self.grid.delete_chars(n);
    }

    fn erase_chars(&mut self, n: u16) {
        let row = self.grid.cursor_row;
        let col = self.grid.cursor_col;
        self.grid
            .clear_cols(row, col, (col + n).min(self.grid.cols));
    }

    fn sgr(&mut self, params: &[u16]) {
        let mut i = 0;
        while i < params.len() {
            match params[i] {
                38 => {
                    // Extended fg color (38;5;N or 38;2;R;G;B)
                    i += 1;
                    if i < params.len() {
                        match params[i] {
                            5 => {
                                i += 1;
                                if i < params.len() {
                                    self.grid.attr.fg_index = params[i] as u8;
                                }
                            }
                            2 if i + 3 < params.len() => {
                                self.grid.attr.fg_index = config::rgb_to_palette(
                                    params[i + 1] as u8,
                                    params[i + 2] as u8,
                                    params[i + 3] as u8,
                                );
                                i += 3;
                            }
                            2 => {}
                            _ => {}
                        }
                    }
                }
                48 => {
                    // Extended bg color (48;5;N or 48;2;R;G;B)
                    i += 1;
                    if i < params.len() {
                        match params[i] {
                            5 => {
                                i += 1;
                                if i < params.len() {
                                    self.grid.attr.bg_index = params[i] as u8;
                                }
                            }
                            2 if i + 3 < params.len() => {
                                self.grid.attr.bg_index = config::rgb_to_palette(
                                    params[i + 1] as u8,
                                    params[i + 2] as u8,
                                    params[i + 3] as u8,
                                );
                                i += 3;
                            }
                            2 => {}
                            _ => {}
                        }
                    }
                }
                code => self.sgr_single(code),
            }
            i += 1;
        }
    }

    #[inline]
    fn sgr_reset(&mut self) {
        self.grid.attr.flags = CellFlags::empty();
        self.grid.attr.fg_index = 7;
        self.grid.attr.bg_index = 0;
    }

    #[inline]
    fn sgr_single(&mut self, code: u16) {
        match code {
            0 => self.sgr_reset(),
            1 => self.grid.attr.flags.insert(CellFlags::BOLD),
            2 => self.grid.attr.flags.insert(CellFlags::DIM),
            3 => self.grid.attr.flags.insert(CellFlags::ITALIC),
            4 => self.grid.attr.flags.insert(CellFlags::UNDERLINE),
            7 => self.grid.attr.flags.insert(CellFlags::INVERSE),
            8 => self.grid.attr.flags.insert(CellFlags::HIDDEN),
            9 => self.grid.attr.flags.insert(CellFlags::STRIKE),
            21 | 22 => {
                self.grid.attr.flags.remove(CellFlags::BOLD);
                self.grid.attr.flags.remove(CellFlags::DIM);
            }
            23 => self.grid.attr.flags.remove(CellFlags::ITALIC),
            24 => self.grid.attr.flags.remove(CellFlags::UNDERLINE),
            27 => self.grid.attr.flags.remove(CellFlags::INVERSE),
            28 => self.grid.attr.flags.remove(CellFlags::HIDDEN),
            29 => self.grid.attr.flags.remove(CellFlags::STRIKE),
            30..=37 => self.grid.attr.fg_index = (code - 30) as u8,
            39 => self.grid.attr.fg_index = 7,
            40..=47 => self.grid.attr.bg_index = (code - 40) as u8,
            49 => self.grid.attr.bg_index = 0,
            90..=97 => self.grid.attr.fg_index = (code - 90 + 8) as u8,
            100..=107 => self.grid.attr.bg_index = (code - 100 + 8) as u8,
            _ => {}
        }
    }

    #[inline]
    fn color_256(&mut self, fg: bool, index: u16) {
        if fg {
            self.grid.attr.fg_index = index as u8;
        } else {
            self.grid.attr.bg_index = index as u8;
        }
    }

    #[inline]
    fn color_rgb(&mut self, fg: bool, r: u16, g: u16, b: u16) {
        let index = config::rgb_to_palette(r as u8, g as u8, b as u8);
        if fg {
            self.grid.attr.fg_index = index;
        } else {
            self.grid.attr.bg_index = index;
        }
    }

    fn sgr_colon(&mut self, raw: &[u8]) {
        // Split on ';' into independent SGR attributes, then split each on ':'
        for attr_bytes in raw.split(|&b| b == b';') {
            let subs: Vec<u16> = attr_bytes
                .split(|&b| b == b':')
                .map(|s| {
                    s.iter().fold(0u32, |acc, &b| {
                        if b.is_ascii_digit() {
                            acc * 10 + (b - b'0') as u32
                        } else {
                            acc
                        }
                    }) as u16
                })
                .collect();

            if subs.is_empty() {
                continue;
            }

            match subs[0] {
                4 => {
                    // Underline style: 4:0=off, 4:1=single, 4:2=double, 4:3=curly, etc.
                    if subs.len() > 1 {
                        if subs[1] == 0 {
                            self.grid.attr.flags.remove(CellFlags::UNDERLINE);
                        } else {
                            // All underline variants → set UNDERLINE (we don't distinguish styles yet)
                            self.grid.attr.flags.insert(CellFlags::UNDERLINE);
                        }
                    } else {
                        self.grid.attr.flags.insert(CellFlags::UNDERLINE);
                    }
                }
                38 => {
                    // Foreground color: 38:5:N or 38:2:[CS]:R:G:B
                    if subs.len() >= 3 && subs[1] == 5 {
                        self.grid.attr.fg_index = subs[2] as u8;
                    } else if subs.len() >= 5 && subs[1] == 2 {
                        let (r, g, b) = if subs.len() >= 6 {
                            (subs[3] as u8, subs[4] as u8, subs[5] as u8)
                        } else {
                            (subs[2] as u8, subs[3] as u8, subs[4] as u8)
                        };
                        self.grid.attr.fg_index = config::rgb_to_palette(r, g, b);
                    }
                }
                48 => {
                    // Background color: 48:5:N or 48:2:[CS]:R:G:B
                    if subs.len() >= 3 && subs[1] == 5 {
                        self.grid.attr.bg_index = subs[2] as u8;
                    } else if subs.len() >= 5 && subs[1] == 2 {
                        let (r, g, b) = if subs.len() >= 6 {
                            (subs[3] as u8, subs[4] as u8, subs[5] as u8)
                        } else {
                            (subs[2] as u8, subs[3] as u8, subs[4] as u8)
                        };
                        self.grid.attr.bg_index = config::rgb_to_palette(r, g, b);
                    }
                }
                58 => {
                    // Underline color — we don't render it separately, ignore
                }
                59 => {
                    // Reset underline color — ignore
                }
                _ => {
                    // Single-value attribute that happens to be in a colon sequence,
                    // e.g. "1" in "1;4:3" — dispatch through normal sgr
                    self.sgr(&subs[..1]);
                }
            }
        }
    }

    fn set_mode(&mut self, params: &[u16], private: bool) {
        for &p in params {
            if private {
                match p {
                    1 => self.grid.mode.insert(TermMode::CURSOR_KEYS),
                    6 => {
                        self.grid.mode.insert(TermMode::ORIGIN_MODE);
                        self.grid.cursor_row = self.grid.scroll_top;
                        self.grid.cursor_col = 0;
                        self.grid.cursor_pending_wrap = false;
                    }
                    7 => self.grid.mode.insert(TermMode::AUTO_WRAP),
                    25 => self.grid.mode.insert(TermMode::CURSOR_VISIBLE),
                    47 | 1047 => self.grid.enter_alt_screen(),
                    1049 => {
                        self.grid.save_cursor();
                        self.grid.enter_alt_screen();
                    }
                    1000 => self.grid.mode.insert(TermMode::MOUSE_BUTTON),
                    1002 => self.grid.mode.insert(TermMode::MOUSE_MOTION),
                    1003 => self.grid.mode.insert(TermMode::MOUSE_ALL),
                    1004 => self.grid.mode.insert(TermMode::FOCUS_EVENTS),
                    1006 => self.grid.mode.insert(TermMode::MOUSE_SGR),
                    2004 => self.grid.mode.insert(TermMode::BRACKETED_PASTE),
                    2026 if !self.grid.mode.contains(TermMode::SYNC_OUTPUT) => {
                        self.grid.mode.insert(TermMode::SYNC_OUTPUT);
                        self.grid.sync_start = Some(Instant::now());
                    }
                    2026 => {}
                    _ => {}
                }
            }
        }
    }

    fn reset_mode(&mut self, params: &[u16], private: bool) {
        for &p in params {
            if private {
                match p {
                    1 => self.grid.mode.remove(TermMode::CURSOR_KEYS),
                    6 => {
                        self.grid.mode.remove(TermMode::ORIGIN_MODE);
                        self.grid.cursor_row = 0;
                        self.grid.cursor_col = 0;
                        self.grid.cursor_pending_wrap = false;
                    }
                    7 => self.grid.mode.remove(TermMode::AUTO_WRAP),
                    25 => self.grid.mode.remove(TermMode::CURSOR_VISIBLE),
                    47 | 1047 => self.grid.exit_alt_screen(),
                    1049 => {
                        self.grid.exit_alt_screen();
                        self.grid.restore_cursor();
                    }
                    1000 => self.grid.mode.remove(TermMode::MOUSE_BUTTON),
                    1002 => self.grid.mode.remove(TermMode::MOUSE_MOTION),
                    1003 => self.grid.mode.remove(TermMode::MOUSE_ALL),
                    1004 => self.grid.mode.remove(TermMode::FOCUS_EVENTS),
                    1006 => self.grid.mode.remove(TermMode::MOUSE_SGR),
                    2004 => self.grid.mode.remove(TermMode::BRACKETED_PASTE),
                    2026 => {
                        self.grid.mode.remove(TermMode::SYNC_OUTPUT);
                        self.grid.sync_start = None;
                    }
                    _ => {}
                }
            }
        }
    }

    fn set_scroll_region(&mut self, top: u16, bottom: u16) {
        let top = top.saturating_sub(1);
        let bottom = if bottom == 0 {
            self.grid.rows - 1
        } else {
            (bottom.saturating_sub(1)).min(self.grid.rows - 1)
        };
        if top < bottom {
            self.grid.scroll_top = top;
            self.grid.scroll_bottom = bottom;
        } else {
            // Invalid region — reset to full screen
            self.grid.scroll_top = 0;
            self.grid.scroll_bottom = self.grid.rows - 1;
        }
        // DECSTBM homes cursor to (1,1). With DECOM, that's top of scroll region.
        if self.grid.mode.contains(TermMode::ORIGIN_MODE) {
            self.grid.cursor_row = self.grid.scroll_top;
        } else {
            self.grid.cursor_row = 0;
        }
        self.grid.cursor_col = 0;
        self.grid.cursor_pending_wrap = false;
    }

    fn tab_clear(&mut self, mode: u16) {
        match mode {
            0 => {
                let col = self.grid.cursor_col as usize;
                if col < self.grid.tab_stops.len() {
                    self.grid.tab_stops.set(col, false);
                }
            }
            3 => self.grid.tab_stops.fill(false),
            _ => {}
        }
    }

    fn set_tab_stop(&mut self) {
        let col = self.grid.cursor_col as usize;
        if col < self.grid.tab_stops.len() {
            self.grid.tab_stops.set(col, true);
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]]) {
        if params.is_empty() {
            return;
        }
        let num = std::str::from_utf8(params[0])
            .ok()
            .and_then(|s| s.parse::<u16>().ok());

        match num {
            Some(0) | Some(2) if params.len() > 1 => {
                let title: Vec<u8> = params[1..].join(&b';');
                self.response_buf.extend_from_slice(b"\x1B]title:");
                self.response_buf.extend_from_slice(&title);
                self.response_buf.push(0x07);
            }
            Some(0) | Some(2) => {}
            Some(52) if params.len() >= 3 => {
                let data = params[2];
                if data.is_empty() {
                    self.response_buf.extend_from_slice(b"\x1B]52;query\x07");
                } else {
                    self.response_buf.extend_from_slice(b"\x1B]52;set:");
                    self.response_buf.extend_from_slice(data);
                    self.response_buf.push(0x07);
                }
            }
            Some(52) => {}
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], byte: u8) {
        match (intermediates, byte) {
            ([], b'7') => self.grid.save_cursor(),
            ([], b'8') => self.grid.restore_cursor(),
            ([], b'D') => self.grid.linefeed(Some(self.scrollback)), // IND
            ([], b'E') => {
                // NEL
                self.grid.carriage_return();
                self.grid.linefeed(Some(self.scrollback));
            }
            ([], b'H') => self.set_tab_stop(),
            ([], b'M') => {
                // RI
                if self.grid.cursor_row == self.grid.scroll_top {
                    self.grid.scroll_down(1);
                } else if self.grid.cursor_row > 0 {
                    self.grid.cursor_row -= 1;
                }
            }
            ([], b'c') => {
                // RIS
                let rows = self.grid.rows;
                self.grid.clear_rows(0, rows);
                self.grid.cursor_row = 0;
                self.grid.cursor_col = 0;
                self.grid.attr = Cell::default();
                self.grid.mode = TermMode::AUTO_WRAP | TermMode::CURSOR_VISIBLE;
                self.grid.scroll_top = 0;
                self.grid.scroll_bottom = rows - 1;
                self.grid.charset_g0 = 0;
                self.grid.charset_g1 = 0;
                self.grid.active_charset = 0;
            }
            ([b'('], b'0') => self.grid.charset_g0 = 1,
            ([b'('], b'B') => self.grid.charset_g0 = 0,
            ([b')'], b'0') => self.grid.charset_g1 = 1,
            ([b')'], b'B') => self.grid.charset_g1 = 0,
            _ => {}
        }
    }

    fn repeat_char(&mut self, n: u16) {
        let c = self.grid.last_char;
        for _ in 0..n {
            self.print(c);
        }
    }

    fn csi_dispatch(&mut self, params: &[u16], intermediates: &[u8], _ignore: bool, byte: u8) {
        // This is the fallback path for CSI sequences that were split across PTY
        // reads (and thus missed the CsiFastParser). For standard no-intermediate
        // sequences, we delegate to the shared dispatch table in CsiFastParser.
        // Only sequences needing response_buf or intermediate-byte handling live here.
        match (intermediates, byte) {
            // DA1
            ([], b'c') => self.response_buf.extend_from_slice(b"\x1B[?6c"),
            // DA2
            ([b'>'], b'c') => self.response_buf.extend_from_slice(b"\x1B[>0;0;0c"),

            // Private mode set/reset
            ([b'?'], b'h') => self.set_mode(params, true),
            ([b'?'], b'l') => self.reset_mode(params, true),

            // DECRQM (DEC private mode query) → respond with DECRPM
            ([b'?', b'$'], b'p') => {
                if let Some(&mode) = params.first() {
                    let pm = match mode {
                        1 | 6 | 7 | 25 | 47 | 1000 | 1002 | 1003 | 1004 | 1006 | 1047 | 1049
                        | 2004 | 2026 => {
                            let flag = match mode {
                                1 => TermMode::CURSOR_KEYS,
                                6 => TermMode::ORIGIN_MODE,
                                7 => TermMode::AUTO_WRAP,
                                25 => TermMode::CURSOR_VISIBLE,
                                47 | 1047 | 1049 => TermMode::ALT_SCREEN,
                                1000 => TermMode::MOUSE_BUTTON,
                                1002 => TermMode::MOUSE_MOTION,
                                1003 => TermMode::MOUSE_ALL,
                                1004 => TermMode::FOCUS_EVENTS,
                                1006 => TermMode::MOUSE_SGR,
                                2004 => TermMode::BRACKETED_PASTE,
                                2026 => TermMode::SYNC_OUTPUT,
                                _ => unreachable!(),
                            };
                            if self.grid.mode.contains(flag) { 1 } else { 2 }
                        }
                        _ => 0,
                    };
                    let resp = format!("\x1B[?{};{}$y", mode, pm);
                    self.response_buf.extend_from_slice(resp.as_bytes());
                }
            }

            // DECSCUSR — cursor style: CSI Ps SP q
            ([b' '], b'q') => {
                let style = params.first().copied().unwrap_or(0);
                self.set_cursor_style(style);
            }

            // Standard CSI sequences — shared dispatch table
            ([], _) => CsiFastParser::dispatch(byte, params, false, self),

            _ => {}
        }
    }

    fn save_cursor(&mut self) {
        self.grid.save_cursor();
    }

    fn restore_cursor(&mut self) {
        self.grid.restore_cursor();
    }

    fn device_status_report(&mut self, mode: u16) {
        match mode {
            5 => self.response_buf.extend_from_slice(b"\x1B[0n"),
            6 => {
                let row = self.grid.cursor_row + 1;
                let col = self.grid.cursor_col + 1;
                let resp = format!("\x1B[{};{}R", row, col);
                self.response_buf.extend_from_slice(resp.as_bytes());
            }
            _ => {}
        }
    }
}
