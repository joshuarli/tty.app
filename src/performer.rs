use crate::config;
use crate::parser::charset::translate_dec_special;
use crate::parser::csi_fast::CsiFastParser;
use crate::parser::perform::Perform;
use crate::perform_shared;
use crate::renderer::atlas::Atlas;
use crate::renderer::font::FontRasterizer;
use crate::terminal::cell::CellFlags;
use crate::terminal::grid::{Grid, TermMode};
use crate::terminal::scrollback::Scrollback;
use crate::unicode::{is_wide, is_zero_width};

/// The performer that bridges parser actions to grid mutations.
pub struct TermPerformer<'a> {
    pub(crate) grid: &'a mut Grid,
    pub(crate) scrollback: &'a mut Scrollback,
    pub(crate) atlas: &'a mut Atlas,
    pub(crate) rasterizer: &'a FontRasterizer,
    pub(crate) response_buf: &'a mut Vec<u8>,
}

impl<'a> TermPerformer<'a> {
    #[allow(dead_code)]
    pub fn new(
        grid: &'a mut Grid,
        scrollback: &'a mut Scrollback,
        atlas: &'a mut Atlas,
        rasterizer: &'a FontRasterizer,
        response_buf: &'a mut Vec<u8>,
    ) -> Self {
        Self {
            grid,
            scrollback,
            atlas,
            rasterizer,
            response_buf,
        }
    }
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
                let bold = self.grid.attr.flags.contains(CellFlags::BOLD);
                let pos = self
                    .atlas
                    .get_or_insert(ch as u32, false, bold, self.rasterizer);
                self.grid.write_char(ch, pos.x, pos.y);
                self.grid.last_char = ch;
                self.grid.last_atlas = [pos.x, pos.y];
            }
        } else {
            // Fast path: bulk write — atlas coords resolved inside Grid from the
            // regular or bold ASCII table.
            self.grid.write_ascii_run(bytes);
            if let Some(&last) = bytes.last() {
                self.grid.last_char = last as char;
                let bold = self.grid.attr.flags.contains(CellFlags::BOLD);
                let ap = self.atlas.get_ascii(last, bold);
                self.grid.last_atlas = [ap.x, ap.y];
            }
        }
    }

    fn print(&mut self, c: char) {
        let cp = c as u32;
        let wide = is_wide(cp);

        if wide {
            let bold = self.grid.attr.flags.contains(CellFlags::BOLD);
            let pos = self.atlas.get_or_insert(cp, true, bold, self.rasterizer);
            self.grid.write_wide_char(c, pos.x, pos.y);
            self.grid.last_char = c;
            self.grid.last_atlas = [pos.x, pos.y];
        } else if is_zero_width(cp) {
            // Zero-width combining marks — ignore for v1
        } else {
            let bold = self.grid.attr.flags.contains(CellFlags::BOLD);
            let pos = self.atlas.get_or_insert(cp, false, bold, self.rasterizer);
            self.grid.write_char(c, pos.x, pos.y);
            self.grid.last_char = c;
            self.grid.last_atlas = [pos.x, pos.y];
        }
    }

    fn execute(&mut self, byte: u8) {
        perform_shared::execute(self.grid, self.scrollback, self.response_buf, byte);
    }

    fn cursor_up(&mut self, n: u16) {
        perform_shared::cursor_up(self.grid, self.scrollback, self.response_buf, n);
    }

    fn cursor_down(&mut self, n: u16) {
        perform_shared::cursor_down(self.grid, self.scrollback, self.response_buf, n);
    }

    fn cursor_forward(&mut self, n: u16) {
        perform_shared::cursor_forward(self.grid, self.scrollback, self.response_buf, n);
    }

    fn cursor_backward(&mut self, n: u16) {
        perform_shared::cursor_backward(self.grid, self.scrollback, self.response_buf, n);
    }

    fn cursor_position(&mut self, row: u16, col: u16) {
        perform_shared::cursor_position(self.grid, self.scrollback, self.response_buf, row, col);
    }

    fn cursor_horizontal_absolute(&mut self, col: u16) {
        perform_shared::cursor_horizontal_absolute(
            self.grid,
            self.scrollback,
            self.response_buf,
            col,
        );
    }

    fn cursor_vertical_absolute(&mut self, row: u16) {
        perform_shared::cursor_vertical_absolute(
            self.grid,
            self.scrollback,
            self.response_buf,
            row,
        );
    }

    fn erase_in_display(&mut self, mode: u16) {
        perform_shared::erase_in_display(self.grid, self.scrollback, self.response_buf, mode);
    }

    fn erase_in_line(&mut self, mode: u16) {
        perform_shared::erase_in_line(self.grid, self.scrollback, self.response_buf, mode);
    }

    fn scroll_up(&mut self, n: u16) {
        perform_shared::scroll_up(self.grid, self.scrollback, self.response_buf, n);
    }

    fn scroll_down(&mut self, n: u16) {
        perform_shared::scroll_down(self.grid, self.scrollback, self.response_buf, n);
    }

    fn insert_lines(&mut self, n: u16) {
        perform_shared::insert_lines(self.grid, self.scrollback, self.response_buf, n);
    }

    fn delete_lines(&mut self, n: u16) {
        perform_shared::delete_lines(self.grid, self.scrollback, self.response_buf, n);
    }

    fn insert_chars(&mut self, n: u16) {
        perform_shared::insert_chars(self.grid, self.scrollback, self.response_buf, n);
    }

    fn delete_chars(&mut self, n: u16) {
        perform_shared::delete_chars(self.grid, self.scrollback, self.response_buf, n);
    }

    fn erase_chars(&mut self, n: u16) {
        perform_shared::erase_chars(self.grid, self.scrollback, self.response_buf, n);
    }

    fn sgr(&mut self, params: &[u16]) {
        perform_shared::sgr(self.grid, self.scrollback, self.response_buf, params);
    }

    #[inline]
    fn sgr_reset(&mut self) {
        perform_shared::sgr_reset(self.grid);
    }

    #[inline]
    fn sgr_single(&mut self, code: u16) {
        perform_shared::sgr_single(self.grid, code);
    }

    #[inline]
    fn color_256(&mut self, fg: bool, index: u16) {
        perform_shared::color_256(self.grid, fg, index);
    }

    #[inline]
    fn color_rgb(&mut self, fg: bool, r: u16, g: u16, b: u16) {
        perform_shared::color_rgb(self.grid, fg, r, g, b);
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
                        self.grid.attr.flags.remove(CellFlags::TRUECOLOR_BG);
                    } else if subs.len() >= 5 && subs[1] == 2 {
                        let (r, g, b) = if subs.len() >= 6 {
                            (subs[3] as u8, subs[4] as u8, subs[5] as u8)
                        } else {
                            (subs[2] as u8, subs[3] as u8, subs[4] as u8)
                        };
                        self.grid.attr.bg_index = config::rgb_to_palette(r, g, b);
                        self.grid.attr.flags.insert(CellFlags::TRUECOLOR_BG);
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
        perform_shared::set_mode(
            self.grid,
            self.scrollback,
            self.response_buf,
            params,
            private,
        );
    }

    fn reset_mode(&mut self, params: &[u16], private: bool) {
        perform_shared::reset_mode(
            self.grid,
            self.scrollback,
            self.response_buf,
            params,
            private,
        );
    }

    fn set_scroll_region(&mut self, top: u16, bottom: u16) {
        perform_shared::set_scroll_region(
            self.grid,
            self.scrollback,
            self.response_buf,
            top,
            bottom,
        );
    }

    fn set_tab_stop(&mut self) {
        perform_shared::set_tab_stop(self.grid, self.scrollback, self.response_buf);
    }

    fn tab_clear(&mut self, mode: u16) {
        perform_shared::tab_clear(self.grid, self.scrollback, self.response_buf, mode);
    }

    fn osc_dispatch(&mut self, params: &[&[u8]]) {
        perform_shared::osc_dispatch(self.grid, self.scrollback, self.response_buf, params);
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], byte: u8) {
        if intermediates.is_empty() && byte == b'H' {
            self.set_tab_stop();
        } else {
            perform_shared::esc_dispatch(
                self.grid,
                self.scrollback,
                self.response_buf,
                intermediates,
                byte,
            );
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
        perform_shared::save_cursor(self.grid);
    }

    fn restore_cursor(&mut self) {
        perform_shared::restore_cursor(self.grid);
    }

    fn device_status_report(&mut self, mode: u16) {
        perform_shared::device_status_report(self.grid, self.scrollback, self.response_buf, mode);
    }
}
